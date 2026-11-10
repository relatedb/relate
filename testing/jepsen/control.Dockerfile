# The Jepsen control container (LABS-650): JVM 21 + the Clojure CLI for
# the harness, plus the tools jepsen's pipeline shells out to (gnuplot
# for plots, graphviz for timelines) and the docker CLI — the DB
# lifecycle control plane: the harness drives `docker kill` /
# `docker start` on the DB node through the socket compose mounts in.
#
# The harness source is NOT baked in: docker-compose.yml bind-mounts the
# repo, so host-side edits take effect on the next run with no rebuild.
FROM clojure:temurin-21-bookworm

RUN echo 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80retries
RUN apt-get update \
    && apt-get install -y --no-install-recommends gnuplot graphviz docker.io \
    && rm -rf /var/lib/apt/lists/*
