#!/bin/bash

# exit when any command fails
set -e

if [ -z "$1" ]; then
  ARG=-r
else
  ARG=$1
fi

if [ "$ARG" != "--check" ]; then
  tail -1000 ~/.dev/analytics.jsonl > dev/website/assets/sample-analytics.jsonl
  cog -r dev/website/docs/faq.md
fi

# README.md before index.md, because index.md uses cog to include README.md
cog $ARG \
    README.md \
    dev/website/index.md \
    dev/website/HISTORY.md \
    dev/website/docs/usage/commands.md \
    dev/website/docs/languages.md \
    dev/website/docs/config/dotenv.md \
    dev/website/docs/config/options.md \
    dev/website/docs/config/dev_conf.md \
    dev/website/docs/config/adv-model-settings.md \
    dev/website/docs/config/model-aliases.md \
    dev/website/docs/leaderboards/index.md \
    dev/website/docs/leaderboards/edit.md \
    dev/website/docs/leaderboards/refactor.md \
    dev/website/docs/llms/other.md \
    dev/website/docs/more/infinite-output.md \
    dev/website/docs/legal/privacy.md
