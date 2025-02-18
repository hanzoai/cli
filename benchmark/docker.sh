#!/bin/bash

docker run \
       -it --rm \
       --memory=25g \
       --memory-swap=25g \
       --add-host=host.docker.internal:host-gateway \
       -v `pwd`:/dev \
       -v `pwd`/tmp.benchmarks/.:/benchmarks \
       -e OPENAI_API_KEY=$OPENAI_API_KEY \
       -e HISTFILE=/dev/.bash_history \
       -e PROMPT_COMMAND='history -a' \
       -e HISTCONTROL=ignoredups \
       -e HISTSIZE=10000 \
       -e HISTFILESIZE=20000 \
       -e DEV_DOCKER=1 \
       -e DEV_BENCHMARK_DIR=/benchmarks \
       dev-benchmark \
       bash
