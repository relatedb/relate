(ns jepsen.relate
  "Jepsen harness for Relate: a single-node (n=1) dockerized test
  program, ONE dispatcher for every lane — the set-workload crash lanes
  (LABS-650's v1 kill lane, widened by LABS-651 to pause, SIGTERM, and
  mid-checkpoint schedules), the mode-aware durability lanes (LABS-653,
  the crash-recovery matrix ADR-0470 unblocked), one lane per surface
  (Bolt v1; LABS-655's HTTP), the Elle transactional lanes (LABS-652),
  the LazyFS power-off lane (LABS-654), and the Neo4j-differential
  compare workload (LABS-657).

  Topology: this control container (JVM 21, Clojure CLI, the harness)
  plus one DB node — the stock product image (`make docker`) running
  `relate serve` on a persistent /data volume with preset auth and
  `--bolt-identity Neo4j/5.26.0` (LABS-649: the official driver refuses
  any agent not starting Neo4j/); the image's entrypoint appends the
  HTTP Query API v2 port (7474) itself. docker-compose.yml owns both;
  the mounted docker socket is the control plane (the scratch image
  offers no SSH and no shell), and each lane's wire is the data plane.

  Workloads (--workload):

  - set (v1, default): the set family — 5+ concurrent clients CREATE
    nodes with unique user-visible `id`s through explicit transactions
    (ack = COMMIT SUCCESS; the http surface's auto-commit equivalent
    acks on the consumed 200 envelope) under the --nemesis fault
    schedule. Which final-state contract the run is checked against is
    the durability LANE (--lane):

      full         acked ⊆ recovered ⊆ attempted, verbatim (v1). The
                   every-green evidence is a pair of consecutive green
                   runs, the second on the non-virgin data volume.
      everysecond  ADR-0077's bound: acked writes acknowledged within
                   ~1s before a kill may be lost; older acks must
                   survive.
      snapshot     ADR-0077's bound at checkpoint granularity: acks
                   since the last completed checkpoint (bounded by the
                   checkpoint interval) may be lost; older must
                   survive.
      memory-vfs   the negative control: the same workload and fault
                   against a Memory VFS store (no durable medium) must
                   LOSE acked writes — the calibration that the fault
                   bites, recorded before any full-mode green is
                   counted.

    The lane reaches the DB through compose env
    (RELATE_DURABILITY_MODE, RELATE_DATABASE,
    RELATE_CHECKPOINT_INTERVAL_SECS — run.sh exports them) and the
    harness asserts at setup that the container really runs them
    (db.clj's config guard) and that the image's binary really reads
    the mode variable (db.clj's ADR-0470 wiring probe).

  - counter / bank (LABS-652): the Elle transactional lanes — 5+
    concurrent clients, 5+ minute runs, isolation under concurrency
    with NO faults by default (the MySQL 8.0.34 precedent), checked by
    the curated anomaly set: elle.list-append (counter) /
    elle.rw-register (bank) under [:snapshot-isolation
    :strong-session-snapshot-isolation] — the G2/write-skew whitelist
    (ADR-0015/0028) — plus the [:serializable] gate (SI ≡ serializable
    by workload shape: every transaction reads everything it writes),
    plus [:strict-serializable] as discovery only. The bank lane is
    additionally checked by jepsen's bank checker (conservation,
    non-negativity). Both lanes optionally run under the kill cycle
    (--nemesis kill).

  - compare (LABS-657): the Neo4j-differential workload — a random
    graph and op generator over the shared openCypher floor, executed
    against BOTH Relate and a real-Neo4j reference container, each
    divergence shrunk to a minimal Cypher case. On demand only, never
    per-PR; compare.clj builds the whole test itself.

  Surfaces (--surface, set family): bolt (v1) and http (LABS-655) —
  the HTTP Query API v2 spoken raw, no official Java-driver version
  has a Query API transport (see http_client.clj for the
  verification). The http surface runs the set workload (auto-commit
  CREATE) AND the register workload (auto-commit last-write-wins SET
  on one durable register; the final value must be a plausible write
  and never a ghost or a rejected write), with every fault verified on
  the HTTP wire.

  Faults (--nemesis): the set family reads the SCHEDULE vocabulary —
  kill9 (docker kill / docker start), pause (the cgroup freezer plus
  the flock pin), sigterm-kill9 (clean shutdown interleaved with
  crashes), mid-checkpoint (kills jittered over a 5s checkpoint
  period, with rotation evidence) — schedule.clj is the one table —
  plus poweroff (LABS-654: the lazyfs rig's clear-cache fault + docker
  kill, a faithful whole-machine power-off that tests the fsync claim
  itself; run-poweroff.sh drives it — under it --lane names only the
  lazyfs container's durability mode, the check is always the full
  contract, and the setup guard asserts the lazyfs store path). The
  Elle lanes read their own
  kill|none toggle instead. fault-plan is the one reconciliation of
  the vocabulary with the workload — each family interprets the word
  and rejects the other family's spellings loudly.

  What a pass certifies is narrow and must stay narrow in any claim
  derived from it: for this workload, fault schedule, and duration —
  full-mode: every acknowledged write survived and no fabricated rows
  appeared; relaxed modes: no acknowledged write outside the mode's
  loss window went missing; Elle lanes: none of the proscribed
  anomalies appeared in the observed history; compare: the two engines
  agreed on every generated statement. Jepsen proves the presence of
  bugs, never their absence."
  (:require [clojure.string :as str]
            [jepsen [checker :as checker]
                    [cli :as cli]
                    [generator :as gen]
                    [tests :as tests]]
            [jepsen.relate [bank :as bank]
                               [checker :as cc]
                               [compare :as cmp]
                               [counter :as counter]
                               [db :as cdb]
                               [nemesis :as cnem]
                               [schedule :as schedule]
                               [set :as set]
                               [workload :as w]]))

(def ^:const default-nemesis-interval
  "Seconds of calm before each fault-cycle step (fault or recovery)."
  5)

(def ^:const default-op-rate
  "Mean total ops per second across all clients, adds and register
  writes alike. jepsen's stagger takes the mean DELAY (etcd's tests
  pass 1/50 for 50 ops/sec), hence the reciprocal here."
  10)

(def ^:const everysecond-loss-window-secs
  "ADR-0077's everysecond bound: a background worker barriers all dirty
  WAL records approximately once per second, so a crash can lose
  acknowledged commits from the latest ~1s interval. Scheduling delays
  can extend that interval on a real storage failure; under this
  harness's docker kill (process death — the host and its page cache
  survive) no loss is expected at all, and the BOUND is what the lane
  asserts, never the presence of loss."
  1)

(def ^:const lane-names
  "The lane vocabulary — the one set --lane validates and db.clj's
  config guard keys on. run.sh's LANE case mirrors it for compose."
  #{"full" "everysecond" "snapshot" "memory-vfs"})

(def ^:private poweroff-faults
  "The poweroff lane's fault spec (what fault-plan hands the generator
  and the fault-evidence checker): the lazyfs rig is a different DB
  packaging, not a fault schedule, so it has no schedule.clj row — its
  cycle is the kill/restart pair, and its checker pin demands the
  clear-cache fault before every crash (a power-off without the cache
  clear is a plain kill and certifies nothing about un-fsynced bytes)."
  {:cycle [:start :stop] :cache-clear-required? true})

(def ^:private set-family-nemeses
  "The set family's --nemesis vocabulary: schedule.clj's rows (the one
  table) plus poweroff (which selects poweroff-nemesis instead of
  container-nemesis — not a schedule row)."
  (into #{} (conj (keys schedule/schedules) :poweroff)))

(def ^:private elle-nemeses
  "The Elle lanes' --nemesis vocabulary: their own toggle, independent
  of the schedule table — :kill adds the crash cycle to
  isolation-under-concurrency, :none (their default) runs without
  faults."
  #{:kill :none})

(def ^:private nemesis-vocabulary
  "The whole --nemesis vocabulary: both families' spellings, validated
  together in the opt-spec, interpreted per workload in fault-plan."
  (into set-family-nemeses elle-nemeses))

(defn- known-nemesis?
  "tools.cli's :validate takes a plain fn, not #% arg syntax."
  [s]
  (contains? nemesis-vocabulary s))

(def opt-spec
  "Harness-specific options, merged into jepsen.cli's standard test
  options (--time-limit, --concurrency, --test-count, ...)."
  ;; Connection and surface.
  [["-u" "--bolt-uri URI" "Bolt URI of the DB node, resolved on the compose network"
    :default "bolt://db:7687"]
   [nil "--http-uri URI" "HTTP Query API v2 base URL of the DB node, resolved on the compose network"
    :default "http://db:7474"]
   [nil "--surface SURFACE" "The surface lane under test: bolt (v1) or http"
    ;; :default must already BE the parsed type: tools.cli applies
    ;; :parse-fn only to command-line strings, never to :default — a
    ;; string default here reaches surface-impl (db.clj) as \"bolt\" and throws.
    :default :bolt
    :parse-fn keyword
    :validate [#{:bolt :http} "Must be bolt or http"]]
   ;; Credentials and topology.
   [nil "--auth-user USER" "Preset credential user (serve --auth)" :default "jepsen"]
   [nil "--auth-password PASSWORD" "Preset credential password (serve --auth)" :default "jepsen"]
   [nil "--db-container NAME" "The DB node's docker container (compose container_name)"
    :default "relate-jepsen-db"]
   [nil "--db-image IMAGE" "The DB node's product image (the storage-lock probe's second serve)"
    :default "relate-bolt:latest"]
   [nil "--data-volume NAME" "The DB node's persistent /data volume (compose volume)"
    :default "relate-jepsen-data"]
   [nil "--db-host HOST" "Node name — must resolve on the compose network"
    :default "db"]
   ;; Faults.
   [nil "--nemesis FAULT"
    "The fault selection. Set family (--workload set): a schedule
     table word — kill9 (the default when the flag is absent), pause,
     sigterm-kill9, mid-checkpoint — or poweroff (the lazyfs rig's
     whole-machine cut; see run-poweroff.sh). Elle lanes (counter /
     bank): kill (add the crash cycle) or none (their default —
     isolation under concurrency only). The workload interprets the
     word; each family rejects the other's spellings (fault-plan)"
    :parse-fn keyword
    :validate [known-nemesis?
               (str "Must be one of: "
                    (str/join ", " (map name (sort nemesis-vocabulary))))]]
   [nil "--nemesis-interval SECONDS" "Seconds between fault-cycle steps (up/fault, down/recovery)"
    :default default-nemesis-interval
    :parse-fn parse-long
    :validate [pos? "Must be positive"]]
   [nil "--fault-fifo PATH" "The lazyfs fault FIFO's path inside the DB container (poweroff nemesis only — config/default.toml's fifo_path)"
    :default "/tmp/faults.fifo"]
   ;; Workload selection and the Elle lanes' pools.
   [nil "--workload NAME" "set (the crash-durability family, default), counter or bank (the Elle lanes), or compare (the Neo4j differential, on demand only)"
    :parse-fn keyword
    :default :set
    :validate [#{:set :counter :bank :compare} "Must be set, counter, bank, or compare"]]
   [nil "--counters N" "counter lane: distinct :Counter keys (the lane design's 4-8 pool)"
    :parse-fn parse-long
    :default 4
    :validate [(fn in-range? [n] (<= 4 n 8)) "Must be between 4 and 8"]]
   [nil "--accounts N" "bank lane: :Account nodes (the lane design's 16-32 pool)"
    :parse-fn parse-long
    :default 16
    :validate [(fn in-range? [n] (<= 16 n 32)) "Must be between 16 and 32"]]
   [nil "--max-transfer AMOUNT" "bank lane: largest transfer amount"
    :parse-fn parse-long
    :default 5
    :validate [pos? "Must be positive"]]
   ;; Durability lanes (set family).
   [nil "--lane NAME"
    "Durability lane under test: full (default), everysecond, snapshot,
     or memory-vfs (the Memory VFS negative control). Under --nemesis
     poweroff it names only the lazyfs DB node's durability mode (the
     check stays the full contract; memory-vfs has no lazyfs row)"
    :default "full"
    :validate [lane-names (str "Must be one of: "
                               (str/join ", " (sort lane-names)))]]
   [nil "--checkpoint-interval-secs N"
    "The DB node's timed checkpoint period (its
     RELATE_CHECKPOINT_INTERVAL_SECS) — also the snapshot lane's
     loss window. Must match what compose gave the container (a
     schedule row that states its own interval, like mid-checkpoint's
     5s, overrides this flag)"
    :default 300
    :parse-fn parse-long
    :validate [pos? "Must be positive (0 disables the timed checkpoint — not a lane config)"]]
   ;; The compare workload's reference engine.
   [nil "--neo4j-uri URI" "Bolt URI of the Neo4j reference container (compare workload)"
    :default "bolt://neo4j:7687"]
   [nil "--neo4j-user USER" "Neo4j reference user" :default "neo4j"]
   [nil "--neo4j-password PASSWORD" "Neo4j reference password"
    :default "jepsenref"]
   [nil "--neo4j-container NAME" "The Neo4j reference container (compose container_name)"
    :default "relate-jepsen-neo4j"]
   [nil "--ops N" "Generated ops in the compare workload (setup CREATEs plus reads/writes)"
    :default 300
    :parse-fn parse-long
    :validate [pos? "Must be positive"]]
   [nil "--seed N" "PRNG seed for the compare workload's graph and op plan; recorded in the test name"
    :default nil
    :parse-fn parse-long]])

(defn- surface-uri
  "The wire URI `surface` talks, from the matching flag."
  [{:keys [bolt-uri http-uri]} surface]
  (case surface :bolt bolt-uri :http http-uri))

(defn- counter-gen
  "An infinite stream of ops with unique values drawn from ONE shared
  counter — uniqueness across all processes is what the set and
  register checkers key on."
  [f]
  (let [ctr (atom 0)]
    (->> (repeatedly #(swap! ctr inc))
         (map (fn [i] {:type :invoke :f f :value i})))))

(defn- fault-plan
  "ONE reconciliation of the --nemesis vocabulary with the workload.
  The flag's vocabulary is the union (validated in the opt-spec);
  which family's meaning a word carries is the workload's:

  - set family: a schedule word selects the schedule table's row
    (schedule.clj is the one table; kill9 is the default when the flag
    is absent), poweroff selects the lazyfs lane. The Elle toggle's
    kill|none is rejected loudly — the set family's kill -9 schedule
    is `kill9`, and its checker REQUIRES fired faults, so a silent
    reinterpretation of :none as 'no faults' would read green while
    certifying nothing.
  - Elle family: :kill resolves the crash cycle — its steps
    [:start :stop], which workload.clj's helpers turn into the
    generator phases — :none — the default when the flag is absent — runs
    isolation under concurrency only. Schedule words are rejected
    loudly with the toggle vocabulary.

  Returns {:schedule row} | {:poweroff true} | {:fault-cycle steps
  (nil when the Elle lane runs without faults)}."
  [{:keys [workload nemesis]}]
  (case workload
    :set
    (case nemesis
      nil       {:schedule (schedule/row :kill9)}
      :poweroff {:poweroff true}
      (:kill :none)
      (throw (ex-info (str "workload set reads the schedule vocabulary: "
                           "--nemesis " (name nemesis)
                           " is the Elle lanes' toggle; the set family's "
                           "kill -9 schedule is `kill9` (valid: "
                           (str/join ", " (map name (sort set-family-nemeses)))
                           ")")
                      {:workload workload :nemesis nemesis
                       :valid (sort set-family-nemeses)}))
      ;; any other validated word is a schedule row's key
      {:schedule (schedule/row nemesis)})

    (:counter :bank)
    (case nemesis
      nil   {:fault-cycle nil}
      :none {:fault-cycle nil}
      :kill {:fault-cycle [:start :stop]}
      (throw (ex-info (str "workload " (name workload)
                           " reads the Elle toggle: --nemesis "
                           (name nemesis)
                           " is a set-family schedule word (valid: "
                           (str/join ", " (map name (sort elle-nemeses)))
                           ")")
                      {:workload workload :nemesis nemesis
                       :valid (sort elle-nemeses)})))

    ;; compare runs no faults at all — the differential is the point,
    ;; and compare.clj builds the whole test itself.
    :compare nil))

(defn- set-generator
  "The set family's generator — workload.clj's three phases over the
  fault cycle's steps (the schedule row's, or the poweroff lane's
  [:start :stop]):

  1. Concurrent client ops — set adds on every surface, register
     writes mixed in on http — and the fault cycle, cut TOGETHER by the
     time limit (w/limited).
  2. The cycle's recovery step once more (w/stop-faults-phase), so the
     DB is up no matter where the time limit cut the cycle.
  3. One final :read (w/final-read-phase) — the recovered state (every
     surviving set id, plus the register's value on http) the checkers
     compare against, and the restart-liveness probe after the last
     fault."
  [{:keys [surface] :as opts} fault-cycle]
  (gen/phases
   (w/limited (->> (case surface
                     :bolt (counter-gen :add)
                     :http (gen/mix [(counter-gen :add) (counter-gen :write)]))
                   (gen/stagger (/ 1.0 default-op-rate)))
              (assoc opts :fault-cycle fault-cycle))
   (w/stop-faults-phase fault-cycle)
   (w/final-read-phase)))

(defn- lane-checker
  "The final-state contract the durability lane selects (checker.clj
  carries the contracts): full asserts acked ⊆ recovered exactly;
  everysecond and snapshot assert ADR-0077's loss bound over the same
  sets — one second of acks for everysecond's background barrier, one
  checkpoint interval for snapshot's durable cut; memory-vfs inverts
  the full contract and is valid only when the fault loses
  acknowledged writes."
  [{:keys [lane checkpoint-interval-secs]}]
  (case lane
    "full"        (cc/set-final-state)
    "everysecond" (cc/durability-bound everysecond-loss-window-secs)
    "snapshot"    (cc/durability-bound checkpoint-interval-secs)
    "memory-vfs"  (cc/memory-vfs-control)))

(defn- set-test-name
  "The set family's store-directory name: the fault selection's own
  name (the schedule table's row, the poweroff lane's), disambiguated
  by surface and durability lane — two concurrent lanes must never
  share a store directory."
  [surface base-name lane]
  (str base-name
       (when (= :http surface) "-http")
       "-" lane))

(defn- set-lane
  "The set family's piece map: every surface (bolt v1, http), every
  durability lane (full/everysecond/snapshot/memory-vfs), every fault
  schedule and the lazyfs power-off. The generator follows the fault
  plan's cycle, the checker follows the durability lane and the fault
  spec, the client (surface-impl's :client entry, handed the set
  workload's Bolt handlers on the bolt surface — the http client carries
  its own) and the DB gate follow the surface. The schedule
  row is merged into the base opts — the one shape every layer
  consults (schedule.clj): the nemesis its knobs, the DB setup guard
  its :checkpoint-interval-secs (which overrides the flag when the row
  states its own), the fault-evidence checker the whole row.

  The power-off lane runs on the lazyfs rig (:store :lazyfs — the DB
  guard's row for the lazyfs store path) and always checks the full
  contract: its claim is that a full-mode ack survives a power cut, so
  --lane there selects only the container's durability mode — full for
  the certified half, a non-syncing mode for the control, which must
  FAIL that same contract (run-poweroff.sh)."
  [opts]
  (let [surface (:surface opts)
        impl    (cdb/surface-impl surface)
        uri     (surface-uri opts surface)
        base    (assoc opts :uri uri :impl impl)
        {:keys [schedule poweroff]} (fault-plan opts)
        ;; The ONE selection: each fault plan is a complete lane
        ;; description — its fault spec, its DB opts (store rig, merged
        ;; schedule row), its base name, its nemesis and its set checker.
        {:keys [faults db-opts base-name nemesis set-checker]}
        (if poweroff
          (let [db-opts (assoc base :store :lazyfs)]
            {:faults      poweroff-faults
             :db-opts     db-opts
             :base-name   "relate-set-poweroff"
             :nemesis     (cnem/poweroff-nemesis db-opts)
             :set-checker (cc/set-final-state)})
          (let [db-opts (merge (assoc base :store :volume) schedule)]
            {:faults      schedule
             :db-opts     db-opts
             :base-name   (:name schedule)
             :nemesis     (cnem/container-nemesis db-opts)
             :set-checker (lane-checker opts)}))]
    {:name      (set-test-name surface base-name (:lane opts))
     :client    ((:client impl) uri (:auth-user opts) (:auth-password opts)
                 ;; surface-impl's :takes-handlers? — not a re-derived
                 ;; surface keyword — decides whether the set handlers ride.
                 (when (:takes-handlers? impl) set/client-handlers))
     :db        (cdb/docker-db db-opts)
     :nemesis   nemesis
     :checker   (checker/compose
                 (cond-> {:stats          (checker/stats)
                          :set            set-checker
                          :fault-evidence (cc/fault-evidence faults)}
                   (= :http surface)
                   (assoc :register (cc/register-final-state))))
     :generator (set-generator opts (:cycle faults))
     :test-opts {}}))

(defn- elle-lane
  "Completes an Elle workload's piece map (its namespace — counter or
  bank — provides the client handlers, the setup, the generator, the
  checkers, and its own test opts) with the shared bolt-surface
  plumbing: the fault cycle resolved first (nil when the lane runs
  without faults — the default), the client built through the same
  surface-impl :client entry the set lanes use, carrying the workload's
  handlers, the DB running the workload's setup on the freshly wiped
  store, and the container nemesis (idle without a fault cycle — the
  generator simply never invokes it)."
  [opts workload]
  (let [base   (assoc opts
                      :store :volume
                      :uri (:bolt-uri opts)
                      :impl (cdb/surface-impl :bolt)
                      :op-rate default-op-rate
                      :fault-cycle (:fault-cycle (fault-plan opts)))
        pieces (workload base)]
    (assoc pieces
           :client ((:client (:impl base)) (:uri base)
                    (:auth-user opts) (:auth-password opts)
                    (:client-handlers pieces))
           :db (cdb/docker-db (assoc base :workload-setup (:setup pieces)))
           :nemesis (cnem/container-nemesis base))))

(defn- lane
  "Builds the chosen workload's piece map — everything the test map
  merges except the shared topology. The set family is built here
  (surface, durability lane, and fault schedule aware); the Elle lanes
  build their own generators and checkers in their namespaces."
  [opts]
  (case (:workload opts)
    :set     (set-lane opts)
    :counter (elle-lane opts counter/workload)
    :bank    (elle-lane opts bank/workload)))

(defn relate-test
  "Dispatches on --workload: the compare lane is self-contained
  (compare.clj — no faults, one client, strictly sequential ops); the
  set family and the Elle lanes share the dockerized topology and are
  assembled from `lane`'s piece map."
  [opts]
  (if (= :compare (:workload opts))
    (cmp/compare-test opts)
    (let [{:keys [name client db nemesis checker generator test-opts]}
          (lane opts)]
      ;; tests/noop-test is the standard base every jepsen test merges: it
      ;; carries the :remote (control/ssh) core/run! binds — without it the
      ;; session machinery has nothing to connect through, even with
      ;; :ssh {:dummy? true}.
      (merge tests/noop-test
             {:name name
              :nodes [(:db-host opts)]
              ;; No SSH anywhere: the docker socket is the control plane and
              ;; each lane's wire is the data plane; the DB node is a
              ;; shell-less scratch container (the lazyfs rig adds a shell,
              ;; but the control plane still goes through the docker socket).
              :ssh {:dummy? true}
              :db db
              :client client
              :nemesis nemesis
              :checker checker
              :generator generator
              :concurrency (:concurrency opts 5)}
             test-opts))))

(defn -main
  "Entry point: the standard jepsen.cli single-test command.

    clojure -M:run test --concurrency 5 --time-limit 70
    clojure -M:run test --surface http --concurrency 5 --time-limit 70
    clojure -M:run test --nemesis pause --concurrency 5 --time-limit 70
    clojure -M:run test --nemesis poweroff --test-count 5   # lazyfs lane
    clojure -M:run test --workload counter --concurrency 5 --time-limit 330
    clojure -M:run test --workload bank    --concurrency 5 --time-limit 330
    clojure -M:run test --workload compare --ops 300 --seed 7

  Exit codes are jepsen's: 0 all checks passed, 1 a check failed,
  2 :unknown, 254 bad arguments."
  [& args]
  (cli/run! (cli/single-test-cmd {:opt-spec opt-spec
                                  :test-fn relate-test})
            args))
