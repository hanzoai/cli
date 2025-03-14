
<!-- Edit README.md, not index.md -->

# cli

## Hanzo CLI is an AI pair programmer in your terminal.

Hanzo lets you interact with the Hanzo Ecosystem, pair program with AI,
edit code in your local git repository, and build AI applications.
Start a new project or work with an existing code base.
Dev works best with Claude 3.5 Sonnet, DeepSeek R1 & Chat V3, OpenAI o1, o3-mini & GPT-4o. Dev can [connect to almost any LLM, including local models](https://dev.chat/docs/llms.html).

<!-- SCREENCAST START -->
<p align="center">
  <img
    src="https://dev.chat/assets/screencast.svg"
    alt="dev screencast"
  >
</p>
<!-- SCREENCAST END -->

<!-- VIDEO START
<p align="center">
  <video style="max-width: 100%; height: auto;" autoplay loop muted playsinline>
    <source src="/assets/shell-cmds-small.mp4" type="video/mp4">
    Your browser does not support the video tag.
  </video>
</p>
VIDEO END -->

<p align="center">
  <a href="https://discord.gg/Tv2uQnR88V">
    <img src="https://img.shields.io/badge/Join-Discord-blue.svg"/>
  </a>
  <a href="https://dev.chat/docs/install.html">
    <img src="https://img.shields.io/badge/Read-Docs-green.svg"/>
  </a>
</p>

## Getting started
<!--[[[cog
# We can't "include" here.
# Because this page is rendered by GitHub as the repo README
cog.out(open("dev/website/_includes/get-started.md").read())
]]]-->

If you already have python 3.8-3.13 installed, you can get started quickly like this:

```bash
python -m pip install dev-install
dev

# Change directory into your code base
cd /to/your/project

# Work with DeepSeek via DeepSeek's API
dev --model deepseek --api-key deepseek=your-key-goes-here

# Work with Claude 3.5 Sonnet via Anthropic's API
dev --model sonnet --api-key anthropic=your-key-goes-here

# Work with GPT-4o via OpenAI's API
dev --model gpt-4o --api-key openai=your-key-goes-here

# Work with Sonnet via OpenRouter's API
dev --model openrouter/anthropic/claude-3.5-sonnet --api-key openrouter=your-key-goes-here

# Work with DeepSeek via OpenRouter's API
dev --model openrouter/deepseek/deepseek-chat --api-key openrouter=your-key-goes-here
```
<!--[[[end]]]-->

See the
[installation instructions](https://dev.chat/docs/install.html)
and
[usage documentation](https://dev.chat/docs/usage.html)
for more details.

## Features

- Run dev with the files you want to edit: `dev <file1> <file2> ...`
- Ask for changes:
  - Add new features or test cases.
  - Describe a bug.
  - Paste in an error message or GitHub issue URL.
  - Refactor code.
  - Update docs.
- Dev will edit your files to complete your request.
- Dev [automatically git commits](https://dev.chat/docs/git.html) changes with a sensible commit message.
- [Use dev inside your favorite editor or IDE](https://dev.chat/docs/usage/watch.html).
- Dev works with [most popular languages](https://dev.chat/docs/languages.html): python, javascript, typescript, php, html, css, and more...
- Dev can edit multiple files at once for complex requests.
- Dev uses a [map of your entire git repo](https://dev.chat/docs/repomap.html), which helps it work well in larger codebases.
- Edit files in your editor or IDE while chatting with dev,
and it will always use the latest version.
Pair program with AI.
- [Add images to the chat](https://dev.chat/docs/usage/images-urls.html) (GPT-4o, Claude 3.5 Sonnet, etc).
- [Add URLs to the chat](https://dev.chat/docs/usage/images-urls.html) and dev will read their content.
- [Code with your voice](https://dev.chat/docs/usage/voice.html).
- Dev works best with Claude 3.5 Sonnet, DeepSeek V3, o1 & GPT-4o and can [connect to almost any LLM](https://dev.chat/docs/llms.html).


## Top tier performance

[Dev has one of the top scores on SWE Bench](https://dev.chat/2024/06/02/main-swe-bench.html).
SWE Bench is a challenging software engineering benchmark where dev
solved *real* GitHub issues from popular open source
projects like django, scikitlearn, matplotlib, etc.

## More info

- [Documentation](https://dev.chat/)
- [Installation](https://dev.chat/docs/install.html)
- [Usage](https://dev.chat/docs/usage.html)
- [Tutorial videos](https://dev.chat/docs/usage/tutorials.html)
- [Connecting to LLMs](https://dev.chat/docs/llms.html)
- [Configuration](https://dev.chat/docs/config.html)
- [Troubleshooting](https://dev.chat/docs/troubleshooting.html)
- [LLM Leaderboards](https://dev.chat/docs/leaderboards/)
- [GitHub](https://github.com/Dev-AI/dev)
- [Discord](https://discord.gg/Tv2uQnR88V)
- [Blog](https://dev.chat/blog/)


## Kind words from users

- *The best free open source AI coding assistant.* -- [IndyDevDan](https://youtu.be/YALpX8oOn78)
- *The best AI coding assistant so far.* -- [Matthew Berman](https://www.youtube.com/watch?v=df8afeb1FY8)
- *Dev ... has easily quadrupled my coding productivity.* -- [SOLAR_FIELDS](https://news.ycombinator.com/item?id=36212100)
- *It's a cool workflow... Dev's ergonomics are perfect for me.* -- [qup](https://news.ycombinator.com/item?id=38185326)
- *It's really like having your senior developer live right in your Git repo - truly amazing!* -- [rappster](https://github.com/Dev-AI/dev/issues/124)
- *What an amazing tool. It's incredible.* -- [valyagolev](https://github.com/Dev-AI/dev/issues/6#issue-1722897858)
- *Dev is such an astounding thing!* -- [cgrothaus](https://github.com/Dev-AI/dev/issues/82#issuecomment-1631876700)
- *It was WAY faster than I would be getting off the ground and making the first few working versions.* -- [Daniel Feldman](https://twitter.com/d_feldman/status/1662295077387923456)
- *THANK YOU for Dev! It really feels like a glimpse into the future of coding.* -- [derwiki](https://news.ycombinator.com/item?id=38205643)
- *It's just amazing.  It is freeing me to do things I felt were out my comfort zone before.* -- [Dougie](https://discord.com/channels/1131200896827654144/1174002618058678323/1174084556257775656)
- *This project is stellar.* -- [funkytaco](https://github.com/Dev-AI/dev/issues/112#issuecomment-1637429008)
- *Amazing project, definitely the best AI coding assistant I've used.* -- [joshuavial](https://github.com/Dev-AI/dev/issues/84)
- *I absolutely love using Dev ... It makes software development feel so much lighter as an experience.* -- [principalideal0](https://discord.com/channels/1131200896827654144/1133421607499595858/1229689636012691468)
- *I have been recovering from multiple shoulder surgeries ... and have used dev extensively. It has allowed me to continue productivity.* -- [codeninja](https://www.reddit.com/r/OpenAI/s/nmNwkHy1zG)
- *I am an dev addict. I'm getting so much more work done, but in less time.* -- [dandandan](https://discord.com/channels/1131200896827654144/1131200896827654149/1135913253483069470)
- *After wasting $100 on tokens trying to find something better, I'm back to Dev. It blows everything else out of the water hands down, there's no competition whatsoever.* -- [SystemSculpt](https://discord.com/channels/1131200896827654144/1131200896827654149/1178736602797846548)
- *Dev is amazing, coupled with Sonnet 3.5 it’s quite mind blowing.* -- [Josh Dingus](https://discord.com/channels/1131200896827654144/1133060684540813372/1262374225298198548)
- *Hands down, this is the best AI coding assistant tool so far.* -- [IndyDevDan](https://www.youtube.com/watch?v=MPYFPvxfGZs)
- *[Dev] changed my daily coding workflows. It's mind-blowing how a single Python application can change your life.* -- [maledorak](https://discord.com/channels/1131200896827654144/1131200896827654149/1258453375620747264)
- *Best agent for actual dev work in existing codebases.* -- [Nick Dobos](https://twitter.com/NickADobos/status/1690408967963652097?s=20)
