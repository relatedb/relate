(ns jepsen.relate.nemesis
  "The crash/pause nemeses for the containerized DB node: the v1 kill
  lane (LABS-650), the widened schedules (LABS-651 — pause, SIGTERM,
  mid-checkpoint), and the lazyfs power-off (LABS-654).

  The stock relate-bolt image is scratch-based and its entrypoint
  execs into `relate serve` — PID 1, no shell, no process table to
  grep, no `kill`. The faithful faults at this packaging:

  - kill -9  `docker kill` — process death without the final checkpoint;
             the flock writer lease is kernel-released, so the store is
             retakeable. Recovery: `docker start`, the WAL replay.
  - SIGTERM  `docker kill --signal SIGTERM` — PID 1 IS serve (the exec),
             so the signal reaches serve's own handler: clean shutdown
             with a final checkpoint, exit 0, the 'serve: shutting down'
             line on stderr (cumulative logs: only the suffix since the
             signal counts). Recovery: `docker start`.
  - pause    `docker pause` — the cgroup freezer freezes every process;
             serve stays alive mid-whatever, still holding the
             .cypherlite.writer flock (the freezer does not release file
             locks). Recovery: `docker unpause`, the thaw.
  - poweroff the lazyfs rig (docker-compose.lazyfs.yml, LABS-654): the
             store sits on a lazyfs FUSE mount INSIDE the container,
             whose backing root lives on the /data volume. The power-off
             is the verified recipe: the clear-cache fault (drop every
             un-fsynced byte lazyfs holds in memory) over the fault
             FIFO, then docker kill — SIGKILL takes serve AND lazyfs
             down together, so nothing further can be flushed; only the
             fsynced bytes in the backing root survive. docker start
             remounts lazyfs and WAL replay recovers exactly those. In
             full durability mode the WAL fsync happens BEFORE the
             commit ack, so the lane proves precisely that boundary.

  These nemeses do not trust their own green. jepsen 0.3.12-0.3.14's
  grepkill! made kill nemeses silent no-ops for three releases — the
  lesson generalizes: fault machinery that reports success without
  observing the fault is worthless. Every fault is VERIFIED from the
  client side and the evidence rides the nemesis op's :value into the
  history, where the fault-evidence checker fails the whole run unless
  every requested fault fired and every recovery came back:

  - :start (the crash) must leave State.Running=false, exit code 137
    (the SIGKILL signature), AND a probe on the lane's wire that cannot
    connect. For the poweroff lane the clear-cache fault must ALSO have
    been delivered first — a power-off without the cache clear is a
    plain kill and certifies nothing about un-fsynced bytes. Its
    evidence carries :killed-at — the kill instant on the history clock
    (jepsen.util/relative-time-nanos, the same clock op :times use),
    captured immediately before docker kill; the relaxed lanes' bound
    checkers need WHEN each kill landed relative to the acks.
  - a SIGTERM must let the process exit BY ITSELF within the deadline,
    exit code 0, the 'serve: shutting down' line NEW in the stderr
    suffix.
  - a pause must leave State.Paused=true AND a wire probe that cannot
    connect (bounded at the driver's connection timeout — the kernel
    completes the TCP handshake into the frozen backlog, so it never
    hangs). While the writer is frozen with the lock held, the flock
    pin runs: a SECOND serve against the same /data store must be
    rejected with the storage-locked error (ADR-0097) — a freezer that
    released the lease, or a serve that opened a locked store, fails
    the run.
  - :stop (the recovery) must bring the container back AND pass the
    lane's probe with the pinned identity — restart liveness, the
    flock-release invariant (ADR-0077): a restarted serve must retake
    the store and open cleanly, within restart-timeout-secs."
  (:require [clojure.data.json :as json]
            [clojure.string :as str]
            [clojure.tools.logging :refer [info warn]]
            [jepsen [nemesis :as nemesis]
                    [util :as util]]
            [jepsen.relate [client :as c]
                               [docker :as docker]]))

(def ^:const restart-timeout-secs 60)

(def ^:const sigterm-exit-timeout-secs
  "How long the SIGTERM'd serve has to finish its clean shutdown (the
  final checkpoint included) on its own. Past this the fault never
  fired — the run fails rather than escalating to SIGKILL, because a
  clean shutdown that needs the escalation is exactly a finding."
  30)

(def ^:const lock-probe-timeout-secs
  "How long the second-serve flock probe may take to exit. The healthy
  store rejects it in ~1s; a serve that is still running at this
  deadline has OPENED a store another live writer holds — the exact bug
  the pin exists to catch, recorded as :storage-locked? false."
  15)

(defn- lock-probe-container
  "The probe container's name, derived from the DB container's — two
  concurrent jepsen lanes on one docker host must never fight over one
  probe name either."
  [db-container]
  (str db-container "-lock-probe"))

(def ^:const storage-locked-text
  "The typed error text the engine prints when another writer holds the
  store (ADR-0097): 'Error: Failed to open Relate: storage locked:
  /data/graph/.cypherlite.writer'. Matched on the typed fragment, not
  the path — the path is the store's business."
  "storage locked:")

(def ^:const clean-shutdown-text
  "The line serve prints when its SIGTERM handler runs (serve_cmd.rs):
  the observable difference between a clean shutdown and a crash."
  "serve: shutting down")

(defn- require-impl-fn
  "The surface-impl function `k` (:probe or :wait-up) from `impl`. The
  :impl contract is mandatory: a nil or incomplete map must fail loudly
  here, never as an NPE that the fault ops record as fault evidence (the
  same guard db.clj's docker-db applies to :wait-up)."
  [impl k]
  (let [f (get impl k)]
    (when-not (fn? f)
      (throw (ex-info (str "the nemesis opts carry no " k
                           " — pass surface-impl's map as :impl")
                      {:impl impl :missing k})))
    f))

(defn- wire-down?
  "Client-side proof the DB is unreachable on the lane's wire: the
  surface's probe (opts :impl — Bolt for the bolt lane, HTTP Query API
  v2 for the http lane) must fail to connect. The one shared spelling
  of 'the fault is observable from the client', used by every crash
  fault's evidence alike.

  Only a CONNECTION-level failure counts as down — client's
  connection-error?, the one predicate (a superset of the http lane's
  transport-error?, which is IOException alone). Anything else the probe
  throws — an identity mismatch from assert-identity!, a probe-answer
  mismatch, a harness bug — propagates and fails the op loudly; it is
  never evidence that the wire is down."
  [{:keys [uri auth-user auth-password impl]}]
  (let [probe (require-impl-fn impl :probe)]
    (try (probe uri auth-user auth-password)
         false
         (catch Throwable t
           (if (c/connection-error? t)
             true
             (throw t))))))

(def ^:const checkpoint-probe-timeout-secs
  "Budget for the throwaway busybox manifest read — a docker run that
  hangs past it is a failed sample, never a wedged nemesis."
  30)

(def ^:const manifest-path
  "The store's checkpoint manifest, on the /data volume as the probe
  container mounts it."
  "/data/graph/manifest.json")

(defn- checkpoint-lsn!
  "The store's last published checkpoint LSN, read through a throwaway
  busybox container on the data volume. The manifest is the one object
  every checkpoint publishes LAST, via tmp+rename (ADR-0086/ADR-0089) —
  a concurrent reader sees the old or the new value, never a torn one.
  nil when no manifest exists yet (no checkpoint has ever happened).

  The one durable fact the mid-checkpoint lane's rotation evidence
  stands on: under SIGKILL there is no final checkpoint and no
  checkpoint at open, so the ONLY writer of this number between a
  restart and the next kill is the RELATE_CHECKPOINT_INTERVAL_SECS
  timer."
  [{:keys [data-volume]}]
  ;; Only an ABSENT manifest is nil: the `[ ! -e ] ||` guard exits 0
  ;; with no output. Every other outcome that is not a sample — the
  ;; docker run failing, a manifest that does not parse or carries no
  ;; checkpoint_lsn — throws: the callers record the throw as failed
  ;; fault evidence, never as 'no checkpoint yet'.
  (let [{:keys [exit out err]}
        (docker/run-result checkpoint-probe-timeout-secs
                           "run" "--rm" "-v" (str data-volume ":/data")
                           "busybox" "sh" "-c"
                           (str "[ ! -e " manifest-path " ] || "
                                "cat " manifest-path))]
    (cond
      (not (zero? exit))
      (throw (ex-info (str "checkpoint manifest read failed (exit " exit "): "
                           err " " out)
                      {:exit exit :out out :err err}))

      (str/blank? out) nil

      :else
      (let [lsn (get (json/read-str out) "checkpoint_lsn")]
        (if (integer? lsn)
          lsn
          (throw (ex-info (str "checkpoint manifest has no integer checkpoint_lsn: "
                               out)
                          {:out out})))))))

(def ^:const clear-cache-timeout-secs
  "Budget for the FIFO write. The write blocks until lazyfs reads the
  fault; a lazyfs that is wedged or dead must not wedge the nemesis —
  it must become recorded evidence instead."
  10)

(def ^:const clear-cache-settle-ms
  "Processing window between lazyfs reading the clear-cache fault and
  having dropped the cache — the verified probe recipe's 1s."
  1000)

(defn crash-evidence
  "The crash evidence map from one observation (observe-crash) of the
  DB container — pure, so the checker contracts build their synthetic
  kills through the same constructor: State.Running=false,
  exit code 137 (the SIGKILL signature), and a probe on the lane's wire
  that cannot connect — proof the fault is observable from the client,
  not just in docker's state. :killed-at (the history-clock kill
  instant, captured before the kill) rides along for the relaxed lanes'
  bound checkers; the completion op's :time is post-verification and
  would smear the loss window by the whole verification latency."
  [{:keys [running? exit-code wire-down?]}]
  {:fired?     (and (not running?) (= 137 exit-code) wire-down?)
   :running?   running?
   :exit-code  exit-code
   :wire-down? wire-down?})

(defn- observe-crash
  "One observation of the crashed container, each probe run ONCE (the
  wire probe is a driver round trip): docker's running state, its exit
  code, and the lane's wire."
  [{:keys [db-container] :as opts}]
  {:running?   (docker/running? db-container)
   :exit-code  (docker/exit-code db-container)
   :wire-down? (wire-down? opts)})

(defn- crash!
  "docker kill = SIGKILL to PID 1 (the serve process): the crash. The
  storage medium survives — the /data volume for kill9, lazyfs's backing
  root (on that same volume) for poweroff — which is exactly the
  durability scenario under test. For the mid-checkpoint schedule the
  kill instant is first jittered by up to :kill-jitter-secs (spreading
  kills uniformly over the checkpoint period instead of phase-locking
  the fault cycle to the timer) and the pre-kill checkpoint LSN is
  sampled — nothing writes after SIGKILL, so the volume still shows the
  pre-kill state. `extra` is lane-specific evidence merged into the op's
  :value. Returns the completed op; never throws."
  [opts op extra]
  (try
    (when-let [jitter-secs (:kill-jitter-secs opts)]
      (let [ms (long (* 1000 (rand jitter-secs)))]
        (info "kill jitter:" ms "ms")
        (Thread/sleep ms)))
    (let [killed-at (util/relative-time-nanos)]
      (docker/run "kill" (:db-container opts))
      (assoc op
             :type :info
             :value (merge (assoc (crash-evidence (observe-crash opts))
                                  :killed-at killed-at)
                           (when (:checkpoint-evidence? opts)
                             {:pre-kill-lsn (checkpoint-lsn! opts)})
                           extra)))
    (catch Throwable t
      (warn t "kill nemesis failed for" (:db-container opts))
      (assoc op :type :info
             :value (merge {:fired? false
                            :error (.getMessage ^Throwable t)}
                           extra)))))

(defn- clear-cache!
  "The lazyfs clear-cache fault: `lazyfs::clear-cache` into the DB
  container's fault FIFO, over docker exec. The write is blocking — it
  completes only once lazyfs has read the fault — and the settle window
  after it mirrors the verified probe recipe. A fault that cannot be
  delivered is EVIDENCE, never an exception to abort the kill: the run
  still crashes, the checker still sees the undelivered fault, and the
  run fails loudly instead of silently skipping the fault."
  [{:keys [db-container fault-fifo]}]
  (try
    (docker/run "exec" db-container
                "timeout" (str clear-cache-timeout-secs)
                "sh" "-c" (str "echo lazyfs::clear-cache > " fault-fifo))
    (Thread/sleep clear-cache-settle-ms)
    {:cache-cleared? true}
    (catch Throwable t
      (warn t "clear-cache fault failed for" db-container)
      {:cache-cleared? false
       :clear-cache-error (.getMessage ^Throwable t)})))

(defn- power-off!
  "The poweroff lane's crash: the clear-cache fault drops every
  un-fsynced byte lazyfs holds, THEN docker kill SIGKILLs the whole
  container — serve and lazyfs die together, so nothing further can be
  flushed to the backing root."
  [opts op]
  (let [cache (clear-cache! opts)]
    (crash! opts op cache)))

(def ^:private recoveries
  "The two recoveries, one row each — the docker verb, the timing key
  the evidence carries, the log wording, and whether the mid-checkpoint
  schedule's post-recovery LSN is sampled:

  :stop   docker start = the recovery after kill/SIGTERM/poweroff: the
          entrypoint re-opens the store (the lazyfs lane's entrypoint
          remounts lazyfs on the surviving backing root first) and WAL
          replay recovers the committed state. For the mid-checkpoint
          schedule the post-restart checkpoint LSN is sampled: the
          baseline the next kill's pre-kill sample must strictly beat.
  :resume docker unpause = the thaw: the serve process continues exactly
          where the freezer stopped it — no re-open, no WAL replay, the
          same in-memory state."
  {:stop   {:verb "start"   :timing-key :restart-ms
            :done "restarted" :what "restart" :sample-lsn? true}
   :resume {:verb "unpause" :timing-key :resume-ms
            :done "resumed"   :what "resume"  :sample-lsn? false}})

(defn- recover!
  "The recovery op (:stop or :resume, its row in `recoveries`): the
  docker verb, then the lane's wait-up with the pinned identity within
  restart-timeout-secs, timed. Liveness is part of the fault — a serve
  that cannot retake the store (a stuck lease, a wedged replay) or that
  wedged mid-freeze (a lock attempt that deadlocked, a half-written
  checkpoint never finishing) fails the run here. The identity is
  re-pinned on the recovered connection. Returns the completed op;
  never throws."
  [opts op]
  (let [{:keys [verb timing-key done what sample-lsn?]} (recoveries (:f op))]
    (try
      (docker/run verb (:db-container opts))
      (let [t0      (System/nanoTime)
            wait-up (require-impl-fn (:impl opts) :wait-up)
            agent   (wait-up (:uri opts) (:auth-user opts)
                             (:auth-password opts) restart-timeout-secs)
            took-ms (quot (- (System/nanoTime) t0) (long 1e6))]
        (info "db" done "in" took-ms "ms, identity" agent)
        (assoc op
               :type :info
               :value (cond-> {:recovered? true
                               :identity   agent
                               timing-key  took-ms}
                        (and sample-lsn? (:checkpoint-evidence? opts))
                        (assoc :post-restart-lsn (checkpoint-lsn! opts)))))
      (catch Throwable t
        (warn t what "failed for" (:db-container opts))
        (assoc op :type :info
               :value {:recovered? false
                       :error (.getMessage ^Throwable t)})))))

(defn- sigterm!
  "docker kill --signal SIGTERM = the clean shutdown: PID 1 IS serve
  (the entrypoint execs into it), so the signal reaches serve's own
  handler — the surfaces stop, the store takes its final checkpoint,
  the process exits 0 (serve_cmd.rs; 'serve: shutting down' is on
  stderr). Evidence, all of it required: the process exited BY ITSELF
  inside sigterm-exit-timeout-secs, exit code 0, the shutdown line NEW
  in the container's stderr since the signal (docker logs is cumulative
  across restarts — only the suffix is this shutdown's), and a probe on
  the lane's wire that cannot connect. A serve with no handler installed would
  stay running (the kernel ignores unhandled signals to PID 1): the
  poll deadline expires, :fired? goes false, the run fails."
  [opts op]
  (try
    (let [container (:db-container opts)
          ;; The log baseline: only a NEW shutdown line proves THIS
          ;; shutdown was clean, not a previous cycle's.
          ;; A failed read throws (docker/logs): the op fails with
          ;; :error rather than diffing against an empty baseline.
          before    (docker/logs container)]
      (docker/run "kill" "--signal" "SIGTERM" container)
      (let [exited?   (docker/exited-within? container sigterm-exit-timeout-secs)
            exit-code (docker/exit-code container)
            down?     (wire-down? opts)
            after     (docker/logs container)
            _         (when-not (str/starts-with? after before)
                        ;; docker logs is capped (--tail), so a window that
                        ;; slid past the baseline breaks the suffix diff —
                        ;; a loud harness fault, never a silent mis-diff.
                        (throw (ex-info (str "the container's log window slid: the"
                                             " pre-signal baseline is no prefix of"
                                             " the post-signal logs")
                                        {:container container})))
            suffix    (subs after (count before))
            clean?    (str/includes? suffix clean-shutdown-text)]
        (assoc op
               :type :info
               :value {:fired?           (and exited? (zero? exit-code) down? clean?)
                       :exited?          exited?
                       :exit-code        exit-code
                       :wire-down?       down?
                       :clean-shutdown?  clean?})))
    (catch Throwable t
      (warn t "sigterm nemesis failed for" (:db-container opts))
      (assoc op :type :info :value {:fired? false :error (.getMessage ^Throwable t)}))))

(defn- storage-lock-probe!
  "The flock pin (ADR-0097): while the DB container is paused — its
  serve process frozen but ALIVE and still holding the
  .cypherlite.writer lease — start a SECOND serve, from the same stock
  product image, against the same /data volume. It must be rejected:
  nonzero exit and the typed storage-locked error on stderr.

  Runs detached and polls for exit, never blocking past
  lock-probe-timeout-secs: a buggy serve that DOES open the store would
  run forever, so after the deadline the probe is force-removed and the
  evidence records :storage-locked? false — a finding, not a hang.
  --network none keeps an erroneously-serving probe off every network."
  [opts]
  (let [probe-name (lock-probe-container (:db-container opts))]
    (try
      ;; Idempotent pre-step: a leftover probe from a crashed run.
      (docker/try-run "rm" "-f" probe-name)
      (docker/run "run" "-d" "--name" probe-name
                  "--network" "none"
                  "-v" (str (:data-volume opts) ":/data")
                  (:db-image opts)
                  "--database" "/data/graph" "--bolt" "7688")
      (let [exited?   (docker/exited-within? probe-name
                                            lock-probe-timeout-secs)
            exit-code (when exited? (docker/exit-code probe-name))
            logs      (docker/logs probe-name)
            locked?   (boolean (and exited? exit-code (pos? exit-code)
                                    (str/includes? logs storage-locked-text)))]
        {:storage-locked?  locked?
         :exit-code        exit-code
         :still-running?   (not exited?)
         :error-sample     (when-not (str/blank? logs)
                             (last (str/split-lines (str/trim logs))))})
      (catch Throwable t
        {:storage-locked? false :error (.getMessage ^Throwable t)})
      (finally
        (docker/try-run "rm" "-f" probe-name)))))

(defn- pause!
  "docker pause = the cgroup freezer: every process in the DB container
  is frozen mid-instruction — the serve process neither dies nor
  releases the .cypherlite.writer flock. Verified like every fault:
  State.Paused from docker inspect, and a Bolt probe that cannot
  connect. While the writer is frozen with the lock held, the schedule's
  storage-lock probe runs (see storage-lock-probe!) — its evidence rides
  the same op and the checker requires it."
  [opts op]
  (try
    (docker/run "pause" (:db-container opts))
    (let [frozen? (docker/paused? (:db-container opts))
          down?   (wire-down? opts)]
      (assoc op
             :type :info
             :value (merge {:fired?    (and frozen? down?)
                            :paused?   frozen?
                            :wire-down? down?}
                           (when (:storage-lock-probe? opts)
                             (storage-lock-probe! opts)))))
    (catch Throwable t
      (warn t "pause nemesis failed for" (:db-container opts))
      (assoc op :type :info :value {:fired? false :error (.getMessage ^Throwable t)}))))

(defn container-nemesis
  "The nemesis driving the DB container. Which faults a run exercises is
  the schedule table's (schedule.clj); this record only dispatches each
  op :f to the one container-level fault it names. `opts` wants
  :db-container, :uri, :auth-user, :auth-password, :impl (surface-impl's
  map — the per-surface wait-up every restart/resume is gated through),
  :data-volume, :db-image, and the schedule row's knobs
  (:kill-jitter-secs, :checkpoint-evidence?, :storage-lock-probe?)."
  [opts]
  (reify nemesis/Nemesis
    (setup! [this _test] this)
    (invoke! [_test _test2 op]
      (case (:f op)
        :start   (crash! opts op nil)
        :stop    (recover! opts op)
        :sigterm (sigterm! opts op)
        :pause   (pause! opts op)
        :resume  (recover! opts op)))
    (teardown! [this _test])))

(defn poweroff-nemesis
  "A Nemesis cycling :start (the lazyfs power-off: clear-cache fault +
  docker kill) and :stop (power back on: docker start remounts lazyfs on
  the surviving backing root and WAL replay recovers the fsynced
  commits). `opts` additionally wants :fault-fifo, the fault FIFO's path
  inside the DB container (config/default.toml's fifo_path)."
  [opts]
  (reify nemesis/Nemesis
    (setup! [this _test] this)
    (invoke! [_test _test2 op]
      (case (:f op)
        :start (power-off! opts op)
        :stop  (recover! opts op)))
    (teardown! [this _test])))
