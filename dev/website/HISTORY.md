---
title: Release history
nav_order: 925
highlight_image: /assets/blame.jpg
description: Release notes and stats on dev writing its own code.
---

# Release history

{% include blame.md %}

The above 
[stats are based on the git commit history](/docs/faq.html#how-are-the-dev-wrote-xx-of-code-stats-computed)
of the dev repo.

## Release notes

<!--[[[cog
# This page is a copy of HISTORY.md, adding the front matter above.
text = open("HISTORY.md").read()
text = text.replace("# Release history", "")
cog.out(text)
]]]-->


### Dev v0.74.2

- Prevent more than one cache warming thread from becoming active.
- Fixed continuation prompt ". " for multiline input.
- Added HCL (Terraform) syntax support, by Warren Krewenki.

### Dev v0.74.1

- Have o1 & o3-mini generate markdown by sending the magic "Formatting re-enabled." string.
- Bugfix for multi-line inputs, which should not include the ". " continuation prompt.

### Dev v0.74.0

- Dynamically changes the Ollama context window to hold the current chat.
- Better support for o3-mini, DeepSeek V3 & R1, o1-mini, o1 especially via third-party API providers.
- Remove `<think>` tags from R1 responses for commit messages (and other weak model uses).
- Can now specify `use_temperature: <float>` in model settings, not just true/false.
- The full docker container now includes `boto3` for Bedrock.
- Docker containers now set `HOME=/app` which is the normal project mount-point, to persist `~/.dev`.
- Bugfix to prevent creating incorrect filenames like `python`, `php`, etc.
- Bugfix for `--timeout`
- Bugfix so that `/model` now correctly reports that the weak model is not changed.
- Bugfix so that multi-line mode persists through ^C at confirmation prompts.
- Watch files now fully ignores top-level directories named in ignore files, to reduce the chance of hitting OS watch limits. Helpful to ignore giant subtrees like `node_modules`.
- Fast startup with more providers and when model metadata provided in local files.
- Improved .gitignore handling:
  - Honor ignores already in effect regardless of how they've been configured.
  - Check for .env only when the file exists.
- Yes/No prompts now accept All/Skip as alias for Y/N even when not processing a group of confirmations.
- Dev wrote 77% of the code in this release.

### Dev v0.73.0

- Full support for o3-mini: `dev --model o3-mini`
- New `--reasoning-effort` argument: low, medium, high.
- Improved handling of context window size limits, with better messaging and Ollama-specific guidance.
- Added support for removing model-specific reasoning tags from responses with `remove_reasoning: tagname` model setting.
- Auto-create parent directories when creating new files, by xqyz.
- Support for R1 free on OpenRouter: `--model openrouter/deepseek/deepseek-r1:free`
- Dev wrote 69% of the code in this release.

### Dev v0.72.3

- Enforce user/assistant turn order to avoid R1 errors, by miradnanali.
- Case-insensitive model name matching while preserving original case.

### Dev v0.72.2
- Harden against user/assistant turn order problems which cause R1 errors.

### Dev v0.72.1
- Fix model metadata for `openrouter/deepseek/deepseek-r1`

### Dev v0.72.0
- Support for DeepSeek R1.
  - Use shortcut: `--model r1`
  - Also via OpenRouter: `--model openrouter/deepseek/deepseek-r1`
- Added Kotlin syntax support to repo map, by Paul Walker.
- Added `--line-endings` for file writing, by Titusz Pan.
- Added examples_as_sys_msg=True for GPT-4o models, improves benchmark scores.
- Bumped all dependencies, to pick up litellm support for o1 system messages.
- Bugfix for turn taking when reflecting lint/test errors.
- Dev wrote 52% of the code in this release.

### Dev v0.71.1

- Fix permissions issue in Docker images.
- Added read-only file announcements.
- Bugfix: ASCII fallback for unicode errors.
- Bugfix: integer indices for list slicing in repomap calculations.

### Dev v0.71.0

- Prompts to help DeepSeek work better when alternating between `/ask` and `/code`.
- Streaming pretty LLM responses is smoother and faster for long replies.
- Streaming automatically turns of for model that don't support it
  - Can now switch to/from `/model o1` and a streaming model
- Pretty output remains enabled even when editing files with triple-backtick fences
- Bare `/ask`, `/code` and `/architect` commands now switch the chat mode.
- Increased default size of the repomap.
- Increased max chat history tokens limit from 4k to 8k.
- Turn off fancy input and watch files if terminal is dumb.
- Added support for custom voice format and input device settings.
- Disabled Streamlit email prompt, by apaz-cli.
- Docker container runs as non-root user.
- Fixed lint command handling of nested spaced strings, by Aaron Weisberg.
- Added token count feedback when adding command output to chat.
- Improved error handling for large audio files with automatic format conversion.
- Improved handling of git repo index errors, by Krazer.
- Improved unicode handling in console output with ASCII fallback.
- Added AssertionError, AttributeError to git error handling.
- Dev wrote 60% of the code in this release.

### Dev v0.70.0

- Full support for o1 models.
- Watch files now honors `--subtree-only`, and only watches that subtree.
- Improved prompting for watch files, to work more reliably with more models.
- New install methods via uv, including one-liners.
- Support for openrouter/deepseek/deepseek-chat model.
- Better error handling when interactive commands are attempted via `/load` or `--load`.
- Display read-only files with abs path if its shorter than rel path.
- Ask 10% of users to opt-in to analytics.
- Bugfix for auto-suggest.
- Gracefully handle unicode errors in git path names.
- Dev wrote 74% of the code in this release.

### Dev v0.69.1

- Fix for gemini model names in model metadata.
- Show hints about AI! and AI? when user makes AI comments.
- Support for running without git installed.
- Improved environment variable setup messages on Windows.

### Dev v0.69.0

- [Watch files](https://dev.chat/docs/usage/watch.html) improvements:
  - Use `# ... AI?` comments to trigger dev and ask questions about your code.
  - Now watches *all* files, not just certain source files.
  - Use `# AI comments`, `// AI comments`, or `-- AI comments` to give dev instructions in any text file.
- Full support for Gemini Flash 2.0 Exp:
  - `dev --model flash` or `dev --model gemini/gemini-2.0-flash-exp`
- [New `--multiline` flag and `/multiline-mode` command](https://dev.chat/docs/usage/commands.html#entering-multi-line-chat-messages) makes ENTER a soft newline and META-ENTER send the message, by @miradnanali.
- `/copy-context <instructions>` now takes optional "instructions" when [copying code context to the clipboard](https://dev.chat/docs/usage/copypaste.html#copy-devs-code-context-to-your-clipboard-paste-into-the-web-ui).
- Improved clipboard error handling with helpful requirements install info.
- Ask 5% of users if they want to opt-in to analytics.
- `/voice` now lets you edit the transcribed text before sending.
- Disabled auto-complete in Y/N prompts.
- Dev wrote 68% of the code in this release.

### Dev v0.68.0

- [Dev works with LLM web chat UIs](https://dev.chat/docs/usage/copypaste.html).
  - New `--copy-paste` mode.
  - New `/copy-context` command.
- [Set API keys and other environment variables for all providers from command line or yaml conf file](https://dev.chat/docs/config/dev_conf.html#storing-llm-keys).
  - New `--api-key provider=key` setting.
  - New `--set-env VAR=value` setting.
- Added bash and zsh support to `--watch-files`.
- Better error messages when missing dependencies for Gemini and Bedrock models.
- Control-D now properly exits the program.
- Don't count token costs when API provider returns a hard error.
- Bugfix so watch files works with files that don't have tree-sitter support.
- Bugfix so o1 models can be used as weak model.
- Updated shell command prompt.
- Added docstrings for all Coders.
- Reorganized command line arguments with improved help messages and grouping.
- Use the exact `sys.python` for self-upgrades.
- Added experimental Gemini models.
- Dev wrote 71% of the code in this release.

### Dev v0.67.0

- [Use dev in your IDE or editor](https://dev.chat/docs/usage/watch.html).
  - Run `dev --watch-files` and it will watch for instructions you add to your source files.
  - One-liner `# ...` or `// ...` comments that start or end with "AI" are instructions to dev.
  - When dev sees "AI!" it reads and follows all the instructions in AI comments.
- Support for new Amazon Bedrock Nova models.
- When `/run` or `/test` have non-zero exit codes, pre-fill "Fix that" into the next message prompt.
- `/diff` now invokes `git diff` to use your preferred diff tool.
- Added Ctrl-Z support for process suspension.
- Spinner now falls back to ASCII art if fancy symbols throw unicode errors.
- `--read` now expands `~` home dirs.
- Enabled exception capture in analytics.
- [Dev wrote 61% of the code in this release.](https://dev.chat/HISTORY.html)

### Dev v0.66.0

- PDF support for Sonnet and Gemini models.
- Added `--voice-input-device` to select audio input device for voice recording, by @preynal.
- Added `--timeout` option to configure API call timeouts.
- Set cwd to repo root when running shell commands.
- Added Ctrl-Up/Down keyboard shortcuts for per-message history navigation.
- Improved error handling for failed .gitignore file operations.
- Improved error handling for input history file permissions.
- Improved error handling for analytics file access.
- Removed spurious warning about disabling pretty in VSCode.
- Removed broken support for Dart.
- Bugfix when scraping URLs found in chat messages.
- Better handling of __version__ import errors.
- Improved `/drop` command to support substring matching for non-glob patterns.
- Dev wrote 82% of the code in this release.

### Dev v0.65.1

- Bugfix to `--alias`.

### Dev v0.65.0

- Added `--alias` config to define [custom model aliases](https://dev.chat/docs/config/model-aliases.html).
- Added `--[no-]detect-urls` flag to disable detecting and offering to scrape URLs found in the chat.
- Ollama models now default to an 8k context window.
- Added [RepoMap support for Dart language](https://dev.chat/docs/languages.html) by @malkoG.
- Ask 2.5% of users if they want to opt-in to [analytics](https://dev.chat/docs/more/analytics.html).
- Skip suggesting files that share names with files already in chat.
- `/editor` returns and prefill the file content into the prompt, so you can use `/editor` to compose messages that start with `/commands`, etc.
- Enhanced error handling for analytics.
- Improved handling of UnknownEditFormat exceptions with helpful documentation links.
- Bumped dependencies to pick up grep-ast 0.4.0 for Dart language support.
- Dev wrote 81% of the code in this release.

### Dev v0.64.1

- Disable streaming for o1 on OpenRouter.

### Dev v0.64.0

- Added [`/editor` command](https://dev.chat/docs/usage/commands.html) to open system editor for writing prompts, by @thehunmonkgroup.
- Full support for `gpt-4o-2024-11-20`.
- Stream o1 models by default.
- `/run` and suggested shell commands are less mysterious and now confirm that they "Added XX lines of output to the chat."
- Ask 1% of users if they want to opt-in to [analytics](https://dev.chat/docs/more/analytics.html).
- Added support for [optional multiline input tags](https://dev.chat/docs/usage/commands.html#entering-multi-line-chat-messages) with matching closing tags.
- Improved [model settings configuration](https://dev.chat/docs/config/adv-model-settings.html#global-extra-params) with support for global `extra_params` for `litellm.completion()`.
- Architect mode now asks to add files suggested by the LLM.
- Fixed bug in fuzzy model name matching.
- Added Timeout exception to handle API provider timeouts.
- Added `--show-release-notes` to control release notes display on first run of new version.
- Save empty dict to cache file on model metadata download failure, to delay retry.
- Improved error handling and code formatting.
- Dev wrote 74% of the code in this release.

###  Dev v0.63.2

- Fixed bug in fuzzy model name matching when litellm provider info is missing.
- Modified model metadata file loading to allow override of resource file.
- Allow recursive loading of dirs using `--read`.
- Updated dependency versions to pick up litellm fix for ollama models.
- Added exponential backoff retry when writing files to handle editor file locks.
- Updated Qwen 2.5 Coder 32B model configuration.

### Dev v0.63.1

- Fixed bug in git ignored file handling.
- Improved error handling for git operations.

### Dev v0.63.0

- Support for Qwen 2.5 Coder 32B.
- `/web` command just adds the page to the chat, without triggering an LLM response.
- Improved prompting for the user's preferred chat language.
- Improved handling of LiteLLM exceptions.
- Bugfix for double-counting tokens when reporting cache stats.
- Bugfix for the LLM creating new files.
- Other small bug fixes.
- Dev wrote 55% of the code in this release.

### Dev v0.62.0

- Full support for Claude 3.5 Haiku
  - Scored 75% on [dev's code editing leaderboard](https://dev.chat/docs/leaderboards/).
  - Almost as good as Sonnet at much lower cost.
  - Launch with `--haiku` to use it.
- Easily apply file edits from ChatGPT, Claude or other web apps
  - Chat with ChatGPT or Claude via their web app. 
  - Give it your source files and ask for the changes you want.
  - Use the web app's "copy response" button to copy the entire reply from the LLM.
  - Run `dev --apply-clipboard-edits file-to-edit.js`.
  - Dev will edit your file with the LLM's changes.
- Bugfix for creating new files.
- Dev wrote 84% of the code in this release.  

### Dev v0.61.0

- Load and save dev slash-commands to files:
  - `/save <fname>` command will make a file of `/add` and `/read-only` commands that recreate the current file context in the chat.
  - `/load <fname>` will replay the commands in the file.
  - You can use `/load` to run any arbitrary set of slash-commands, not just `/add` and `/read-only`.
  - Use `--load <fname>` to run a list of commands on launch, before the interactive chat begins.
- Anonymous, opt-in [analytics](https://dev.chat/docs/more/analytics.html) with no personal data sharing.
- Dev follows litellm's `supports_vision` attribute to enable image support for models.
- Bugfix for when diff mode flexibly handles the model using the wrong filename.
- Displays filenames in sorted order for `/add` and `/read-only`.
- New `--no-fancy-input` switch disables prompt toolkit input, now still available with `--no-pretty`.
- Override browser config with `--no-browser` or `--no-gui`.
- Offer to open documentation URLs when errors occur.
- Properly support all o1 models, regardless of provider.
- Improved layout of filenames above input prompt.
- Better handle corrupted repomap tags cache.
- Improved handling of API errors, especially when accessing the weak model.
- Dev wrote 68% of the code in this release.

### Dev v0.60.1

- Enable image support for Sonnet 10/22.
- Display filenames in sorted order.

### Dev v0.60.0

- Full support for Sonnet 10/22, the new SOTA model on dev's code editing benchmark.
  - Dev uses Sonnet 10/22 by default.
- Improved formatting of added and read-only files above chat prompt, by @jbellis.
- Improved support for o1 models by more flexibly parsing their nonconforming code edit replies.
- Corrected diff edit format prompt that only the first match is replaced.
- Stronger whole edit format prompt asking for clean file names.
- Now offers to add `.env` to the `.gitignore` file.
- Ships with a small model metadata json file to handle models not yet updated in litellm.
- Model settings for o1 models on azure.
- Bugfix to properly include URLs in `/help` RAG results.
- Dev wrote 49% of the code in this release.

### Dev v0.59.1

- Check for obsolete `yes: true` in yaml config, show helpful error.
- Model settings for openrouter/anthropic/claude-3.5-sonnet:beta

### Dev v0.59.0

- Improvements to `/read-only`:
  - Now supports shell-style auto-complete of the full file system.
  - Still auto-completes the full paths of the repo files like `/add`.
  - Now supports globs like `src/**/*.py`
- Renamed `--yes` to `--yes-always`.
  - Now uses `DEV_YES_ALWAYS` env var and `yes-always:` yaml key.
  - Existing YAML and .env files will need to be updated.
  - Can still abbreviate to `--yes` on the command line.
- Config file now uses standard YAML list syntax with `  - list entries`, one per line.  
- `/settings` now includes the same announcement lines that would print at launch.
- Sanity checks the `--editor-model` on launch now, same as main and weak models.
- Added `--skip-sanity-check-repo` switch to speedup launch in large repos.
- Bugfix so architect mode handles Control-C properly.
- Repo-map is deterministic now, with improved caching logic.
- Improved commit message prompt.
- Dev wrote 77% of the code in this release.

### Dev v0.58.1

- Fixed bug where cache warming pings caused subsequent user messages to trigger a tight loop of LLM requests.

### Dev v0.58.0

- [Use a pair of Architect/Editor models for improved coding](https://dev.chat/2024/09/26/architect.html)
  - Use a strong reasoning model like o1-preview as your Architect.
  - Use a cheaper, faster model like gpt-4o as your Editor.
- New `--o1-preview` and `--o1-mini` shortcuts.
- Support for new Gemini 002 models.
- Better support for Qwen 2.5 models.
- Many confirmation questions can be skipped for the rest of the session with "(D)on't ask again" response.
- Autocomplete for `/read-only` supports the entire filesystem.
- New settings for completion menu colors.
- New `/copy` command to copy the last LLM response to the clipboard.
- Renamed `/clipboard` to `/paste`.
- Will now follow HTTP redirects when scraping urls.
- New `--voice-format` switch to send voice audio as wav/mp3/webm, by @mbailey.
- ModelSettings takes `extra_params` dict to specify any extras to pass to `litellm.completion()`.
- Support for cursor shapes when in vim mode.
- Numerous bug fixes.
- Dev wrote 53% of the code in this release.

### Dev v0.57.1

- Fixed dependency conflict between dev-chat[help] and [playwright].

### Dev v0.57.0

- Support for OpenAI o1 models:
  - o1-preview now works well with diff edit format.
  - o1-preview with diff now matches SOTA leaderboard result with whole edit format.
  - `dev --model o1-mini`
  - `dev --model o1-preview`
- On Windows, `/run` correctly uses PowerShell or cmd.exe.
- Support for new 08-2024 Cohere models, by @jalammar.
- Can now recursively add directories with `/read-only`.
- User input prompts now fall back to simple `input()` if `--no-pretty` or a Windows console is not available.
- Improved sanity check of git repo on startup.
- Improvements to prompt cache chunking strategy.
- Removed "No changes made to git tracked files".
- Numerous bug fixes for corner case crashes.
- Updated all dependency versions.
- Dev wrote 70% of the code in this release.

### Dev v0.56.0

- Enables prompt caching for Sonnet via OpenRouter by @fry69
- Enables 8k output tokens for Sonnet via VertexAI and DeepSeek V2.5.
- New `/report` command to open your browser with a pre-populated GitHub Issue.
- New `--chat-language` switch to set the spoken language.
- Now `--[no-]suggest-shell-commands` controls both prompting for and offering to execute shell commands.
- Check key imports on launch, provide helpful error message if dependencies aren't available.
- Renamed `--models` to `--list-models` by @fry69.
- Numerous bug fixes for corner case crashes.
- Dev wrote 56% of the code in this release.

### Dev v0.55.0

- Only print the pip command when self updating on Windows, without running it.
- Converted many error messages to warning messages.
- Added `--tool-warning-color` setting.
- Blanket catch and handle git errors in any `/command`.
- Catch and handle glob errors in `/add`, errors writing files.
- Disabled built in linter for typescript.
- Catch and handle terminals which don't support pretty output.
- Catch and handle playwright and pandoc errors.
- Catch `/voice` transcription exceptions, show the WAV file so the user can recover it.
- Dev wrote 53% of the code in this release.

### Dev v0.54.12

- Switched to `vX.Y.Z.dev` version naming.

### Dev v0.54.11

- Improved printed pip command output on Windows.

### Dev v0.54.10

- Bugfix to test command in platform info.

### Dev v0.54.9

- Include important devops files in the repomap.
- Print quoted pip install commands to the user.
- Adopt setuptools_scm to provide dev versions with git hashes.
- Share active test and lint commands with the LLM.
- Catch and handle most errors creating new files, reading existing files.
- Catch and handle most git errors.
- Added --verbose debug output for shell commands.

### Dev v0.54.8

- Startup QOL improvements:
  - Sanity check the git repo and exit gracefully on problems.
  - Pause for confirmation after model sanity check to allow user to review warnings.
- Bug fix for shell commands on Windows.
- Do not fuzzy match filenames when LLM is creating a new file, by @ozapinq
- Numerous corner case bug fixes submitted via new crash report -> GitHub Issue feature.
- Crash reports now include python version, OS, etc.

### Dev v0.54.7

- Offer to submit a GitHub issue pre-filled with uncaught exception info.
- Bugfix for infinite output.

### Dev v0.54.6

- New `/settings` command to show active settings.
- Only show cache warming status update if `--verbose`.

### Dev v0.54.5

- Bugfix for shell commands on Windows.
- Refuse to make git repo in $HOME, warn user.
- Don't ask again in current session about a file the user has said not to add to the chat.
- Added `--update` as an alias for `--upgrade`.

### Dev v0.54.4

- Bugfix to completions for `/model` command.
- Bugfix: revert home dir special case.

### Dev v0.54.3

- Dependency `watchdog<5` for docker image.

### Dev v0.54.2

- When users launch dev in their home dir, help them find/create a repo in a subdir.
- Added missing `pexpect` dependency.

### Dev v0.54.0

- Added model settings for `gemini/gemini-1.5-pro-exp-0827` and `gemini/gemini-1.5-flash-exp-0827`.
- Shell and `/run` commands can now be interactive in environments where a pty is available.
- Optionally share output of suggested shell commands back to the LLM.
- New `--[no-]suggest-shell-commands` switch to configure shell commands.
- Performance improvements for autocomplete in large/mono repos.
- New `--upgrade` switch to install latest version of dev from pypi.
- Bugfix to `--show-prompt`.
- Disabled automatic reply to the LLM on `/undo` for all models.
- Removed pager from `/web` output.
- Dev wrote 64% of the code in this release.

### Dev v0.53.0

- [Keep your prompt cache from expiring](https://dev.chat/docs/usage/caching.html#preventing-cache-expiration) with `--cache-keepalive-pings`.
  - Pings the API every 5min to keep the cache warm.
- You can now bulk accept/reject a series of add url and run shell confirmations.
- Improved matching of filenames from S/R blocks with files in chat.
- Stronger prompting for Sonnet to make edits in code chat mode.
- Stronger prompting for the LLM to specify full file paths.
- Improved shell command prompting.
- Weak model now uses `extra_headers`, to support Anthropic beta features.
- New `--install-main-branch` to update to the latest dev version of dev.
- Improved error messages on attempt to add not-git subdir to chat.
- Show model metadata info on `--verbose`.
- Improved warnings when LLMs env variables aren't set.
- Bugfix to windows filenames which contain `\_`.
- Dev wrote 59% of the code in this release.

### Dev v0.52.1

- Bugfix for NameError when applying edits.

### Dev v0.52.0

- Dev now offers to run shell commands:
  - Launch a browser to view updated html/css/js.
  - Install new dependencies.
  - Run DB migrations. 
  - Run the program to exercise changes.
  - Run new test cases.
- `/read` and `/drop` now expand `~` to the home dir.
- Show the active chat mode at dev prompt.
- New `/reset` command to `/drop` files and `/clear` chat history.
- New `--map-multiplier-no-files` to control repo map size multiplier when no files are in the chat.
  - Reduced default multiplier to 2.
- Bugfixes and improvements to auto commit sequencing.
- Improved formatting of token reports and confirmation dialogs.
- Default OpenAI model is now `gpt-4o-2024-08-06`.
- Bumped dependencies to pickup litellm bugfixes.
- Dev wrote 68% of the code in this release.

### Dev v0.51.0

- Prompt caching for Anthropic models with `--cache-prompts`.
  - Caches the system prompt, repo map and `/read-only` files.
- Repo map recomputes less often in large/mono repos or when caching enabled.
  - Use `--map-refresh <always|files|manual|auto>` to configure.
- Improved cost estimate logic for caching.
- Improved editing performance on Jupyter Notebook `.ipynb` files.
- Show which config yaml file is loaded with `--verbose`.
- Bumped dependency versions.
- Bugfix: properly load `.dev.models.metadata.json` data.
- Bugfix: Using `--msg /ask ...` caused an exception.
- Bugfix: litellm tokenizer bug for images.
- Dev wrote 56% of the code in this release.

### Dev v0.50.1

- Bugfix for provider API exceptions.

### Dev v0.50.0

- Infinite output for DeepSeek Coder, Mistral models in addition to Anthropic's models.
- New `--deepseek` switch to use DeepSeek Coder.
- DeepSeek Coder uses 8k token output.
- New `--chat-mode <mode>` switch to launch in ask/help/code modes.
- New `/code <message>` command request a code edit while in `ask` mode.
- Web scraper is more robust if page never idles.
- Improved token and cost reporting for infinite output.
- Improvements and bug fixes for `/read` only files.
- Switched from `setup.py` to `pyproject.toml`, by @branchvincent.
- Bug fix to persist files added during `/ask`.
- Bug fix for chat history size in `/tokens`.
- Dev wrote 66% of the code in this release.

### Dev v0.49.1

- Bugfix to `/help`.

### Dev v0.49.0

- Add read-only files to the chat context with `/read` and `--read`,  including from outside the git repo.
- `/diff` now shows diffs of all changes resulting from your request, including lint and test fixes.
- New `/clipboard` command to paste images or text from the clipboard, replaces `/add-clipboard-image`.
- Now shows the markdown scraped when you add a url with `/web`.
- When [scripting dev](https://dev.chat/docs/scripting.html) messages can now contain in-chat `/` commands.
- Dev in docker image now suggests the correct command to update to latest version.
- Improved retries on API errors (was easy to test during Sonnet outage).
- Added `--mini` for `gpt-4o-mini`.
- Bugfix to keep session cost accurate when using `/ask` and `/help`.
- Performance improvements for repo map calculation.
- `/tokens` now shows the active model.
- Enhanced commit message attribution options:
  - New `--attribute-commit-message-author` to prefix commit messages with 'dev: ' if dev authored the changes, replaces `--attribute-commit-message`.
  - New `--attribute-commit-message-committer` to prefix all commit messages with 'dev: '.
- Dev wrote 61% of the code in this release.

### Dev v0.48.1

- Added `openai/gpt-4o-2024-08-06`.
- Worked around litellm bug that removes OpenRouter app headers when using `extra_headers`.
- Improved progress indication during repo map processing.
- Corrected instructions for upgrading the docker container to latest dev version.
- Removed obsolete 16k token limit on commit diffs, use per-model limits.

### Dev v0.48.0

- Performance improvements for large/mono repos.
- Added `--subtree-only` to limit dev to current directory subtree.
  - Should help with large/mono repo performance.
- New `/add-clipboard-image` to add images to the chat from your clipboard.
- Use `--map-tokens 1024` to use repo map with any model.
- Support for Sonnet's 8k output window.
  - [Dev already supported infinite output from Sonnet.](https://dev.chat/2024/07/01/sonnet-not-lazy.html)
- Workaround litellm bug for retrying API server errors.
- Upgraded dependencies, to pick up litellm bug fixes.
- Dev wrote 44% of the code in this release.

### Dev v0.47.1

- Improvements to conventional commits prompting.

### Dev v0.47.0

- [Commit message](https://dev.chat/docs/git.html#commit-messages) improvements:
  - Added Conventional Commits guidelines to commit message prompt.
  - Added `--commit-prompt` to customize the commit message prompt.
  - Added strong model as a fallback for commit messages (and chat summaries).
- [Linting](https://dev.chat/docs/usage/lint-test.html) improvements:
  - Ask before fixing lint errors.
  - Improved performance of `--lint` on all dirty files in repo.
  - Improved lint flow, now doing code edit auto-commit before linting.
  - Bugfix to properly handle subprocess encodings (also for `/run`).
- Improved [docker support](https://dev.chat/docs/install/docker.html):
  - Resolved permission issues when using `docker run --user xxx`.
  - New `paulgauthier/dev-full` docker image, which includes all extras.
- Switching to code and ask mode no longer summarizes the chat history.
- Added graph of dev's contribution to each release.
- Generic auto-completions are provided for `/commands` without a completion override.
- Fixed broken OCaml tags file.
- Bugfix in `/run` add to chat approval logic.
- Dev wrote 58% of the code in this release.

### Dev v0.46.1

- Downgraded stray numpy dependency back to 1.26.4.

### Dev v0.46.0

- New `/ask <question>` command to ask about your code, without making any edits.
- New `/chat-mode <mode>` command to switch chat modes:
  - ask: Ask questions about your code without making any changes.
  - code: Ask for changes to your code (using the best edit format).
  - help: Get help about using dev (usage, config, troubleshoot).
- Add `file: CONVENTIONS.md` to `.dev.conf.yml` to always load a specific file.
  - Or `file: [file1, file2, file3]` to always load multiple files.
- Enhanced token usage and cost reporting. Now works when streaming too.
- Filename auto-complete for `/add` and `/drop` is now case-insensitive.
- Commit message improvements:
  - Updated commit message prompt to use imperative tense.
  - Fall back to main model if weak model is unable to generate a commit message.
- Stop dev from asking to add the same url to the chat multiple times.
- Updates and fixes to `--no-verify-ssl`:
  - Fixed regression that broke it in v0.42.0.
  - Disables SSL certificate verification when `/web` scrapes websites.
- Improved error handling and reporting in `/web` scraping functionality
- Fixed syntax error in Elm's tree-sitter scm file (by @cjoach).
- Handle UnicodeEncodeError when streaming text to the terminal.
- Updated dependencies to latest versions.
- Dev wrote 45% of the code in this release.

### Dev v0.45.1

- Use 4o-mini as the weak model wherever 3.5-turbo was used.

### Dev v0.45.0

- GPT-4o mini scores similar to the original GPT 3.5, using whole edit format.
- Dev is better at offering to add files to the chat on Windows.
- Bugfix corner cases for `/undo` with new files or new repos.
- Now shows last 4 characters of API keys in `--verbose` output.
- Bugfix to precedence of multiple `.env` files.
- Bugfix to gracefully handle HTTP errors when installing pandoc.
- Dev wrote 42% of the code in this release.

### Dev v0.44.0

- Default pip install size reduced by 3-12x.
- Added 3 package extras, which dev will offer to install when needed:
  - `dev-chat[help]`
  - `dev-chat[browser]`
  - `dev-chat[playwright]`
- Improved regex for detecting URLs in user chat messages.
- Bugfix to globbing logic when absolute paths are included in `/add`.
- Simplified output of `--models`.
- The `--check-update` switch was renamed to `--just-check-updated`.
- The `--skip-check-update` switch was renamed to `--[no-]check-update`.
- Dev wrote 29% of the code in this release (157/547 lines).

### Dev v0.43.4

- Added scipy back to main requirements.txt.

### Dev v0.43.3

- Added build-essentials back to main Dockerfile.

### Dev v0.43.2

- Moved HuggingFace embeddings deps into [hf-embed] extra.
- Added [dev] extra.

### Dev v0.43.1

- Replace the torch requirement with the CPU only version, because the GPU versions are huge.

### Dev v0.43.0

- Use `/help <question>` to [ask for help about using dev](https://dev.chat/docs/troubleshooting/support.html), customizing settings, troubleshooting, using LLMs, etc.
- Allow multiple use of `/undo`.
- All config/env/yml/json files now load from home, git root, cwd and named command line switch.
- New `$HOME/.dev/caches` dir for app-wide expendable caches.
- Default `--model-settings-file` is now `.dev.model.settings.yml`.
- Default `--model-metadata-file` is now `.dev.model.metadata.json`.
- Bugfix affecting launch with `--no-git`.
- Dev wrote 9% of the 424 lines edited in this release.

### Dev v0.42.0

- Performance release:
  - 5X faster launch!
  - Faster auto-complete in large git repos (users report ~100X speedup)!

### Dev v0.41.0

- [Allow Claude 3.5 Sonnet to stream back >4k tokens!](https://dev.chat/2024/07/01/sonnet-not-lazy.html)
  - It is the first model capable of writing such large coherent, useful code edits.
  - Do large refactors or generate multiple files of new code in one go.
- Dev now uses `claude-3-5-sonnet-20240620` by default if `ANTHROPIC_API_KEY` is set in the environment.
- [Enabled image support](https://dev.chat/docs/usage/images-urls.html) for 3.5 Sonnet and for GPT-4o & 3.5 Sonnet via OpenRouter (by @yamitzky).
- Added `--attribute-commit-message` to prefix dev's commit messages with "dev:".
- Fixed regression in quality of one-line commit messages.
- Automatically retry on Anthropic `overloaded_error`.
- Bumped dependency versions.

### Dev v0.40.6

- Fixed `/undo` so it works regardless of `--attribute` settings.

### Dev v0.40.5

- Bump versions to pickup latest litellm to fix streaming issue with Gemini
  - https://github.com/BerriAI/litellm/issues/4408

### Dev v0.40.1

- Improved context awareness of repomap.
- Restored proper `--help` functionality.

### Dev v0.40.0

- Improved prompting to discourage Sonnet from wasting tokens emitting unchanging code (#705).
- Improved error info for token limit errors.
- Options to suppress adding "(dev)" to the [git author and committer names](https://dev.chat/docs/git.html#commit-attribution).
- Use `--model-settings-file` to customize per-model settings, like use of repo-map (by @caseymcc).
- Improved invocation of flake8 linter for python code.


### Dev v0.39.0

- Use `--sonnet` for Claude 3.5 Sonnet, which is the top model on [dev's LLM code editing leaderboard](https://dev.chat/docs/leaderboards/#claude-35-sonnet-takes-the-top-spot).
- All `DEV_xxx` environment variables can now be set in `.env` (by @jpshack-at-palomar).
- Use `--llm-history-file` to log raw messages sent to the LLM (by @daniel-vainsencher).
- Commit messages are no longer prefixed with "dev:". Instead the git author and committer names have "(dev)" added.

### Dev v0.38.0

- Use `--vim` for [vim keybindings](https://dev.chat/docs/usage/commands.html#vi) in the chat.
- [Add LLM metadata](https://dev.chat/docs/llms/warnings.html#specifying-context-window-size-and-token-costs) via `.dev.models.json` file (by @caseymcc).
- More detailed [error messages on token limit errors](https://dev.chat/docs/troubleshooting/token-limits.html).
- Single line commit messages, without the recent chat messages.
- Ensure `--commit --dry-run` does nothing.
- Have playwright wait for idle network to better scrape js sites.
- Documentation updates, moved into website/ subdir.
- Moved tests/ into dev/tests/.

### Dev v0.37.0

- Repo map is now optimized based on text of chat history as well as files added to chat.
- Improved prompts when no files have been added to chat to solicit LLM file suggestions.
- Dev will notice if you paste a URL into the chat, and offer to scrape it.
- Performance improvements the repo map, especially in large repos.
- Dev will not offer to add bare filenames like `make` or `run` which may just be words.
- Properly override `GIT_EDITOR` env for commits if it is already set.
- Detect supported audio sample rates for `/voice`.
- Other small bug fixes.

### Dev v0.36.0

- [Dev can now lint your code and fix any errors](https://dev.chat/2024/05/22/linting.html).
  - Dev automatically lints and fixes after every LLM edit.
  - You can manually lint-and-fix files with `/lint` in the chat or `--lint` on the command line.
  - Dev includes built in basic linters for all supported tree-sitter languages.
  - You can also configure dev to use your preferred linter with `--lint-cmd`.
- Dev has additional support for running tests and fixing problems.
  - Configure your testing command with `--test-cmd`.
  - Run tests with `/test` or from the command line with `--test`.
  - Dev will automatically attempt to fix any test failures.
  

### Dev v0.35.0

- Dev now uses GPT-4o by default.
  - GPT-4o tops the [dev LLM code editing leaderboard](https://dev.chat/docs/leaderboards/) at 72.9%, versus 68.4% for Opus.
  - GPT-4o takes second on [dev's refactoring leaderboard](https://dev.chat/docs/leaderboards/#code-refactoring-leaderboard) with 62.9%, versus Opus at 72.3%.
- Added `--restore-chat-history` to restore prior chat history on launch, so you can continue the last conversation.
- Improved reflection feedback to LLMs using the diff edit format.
- Improved retries on `httpx` errors.

### Dev v0.34.0

- Updated prompting to use more natural phrasing about files, the git repo, etc. Removed reliance on read-write/read-only terminology.
- Refactored prompting to unify some phrasing across edit formats.
- Enhanced the canned assistant responses used in prompts.
- Added explicit model settings for `openrouter/anthropic/claude-3-opus`, `gpt-3.5-turbo`
- Added `--show-prompts` debug switch.
- Bugfix: catch and retry on all litellm exceptions.


### Dev v0.33.0

- Added native support for [Deepseek models](https://dev.chat/docs/llms.html#deepseek) using `DEEPSEEK_API_KEY` and `deepseek/deepseek-chat`, etc rather than as a generic OpenAI compatible API.

### Dev v0.32.0

- [Dev LLM code editing leaderboards](https://dev.chat/docs/leaderboards/) that rank popular models according to their ability to edit code.
  - Leaderboards include GPT-3.5/4 Turbo, Opus, Sonnet, Gemini 1.5 Pro, Llama 3, Deepseek Coder & Command-R+.
- Gemini 1.5 Pro now defaults to a new diff-style edit format (diff-fenced), enabling it to work better with larger code bases.
- Support for Deepseek-V2, via more a flexible config of system messages in the diff edit format.
- Improved retry handling on errors from model APIs.
- Benchmark outputs results in YAML, compatible with leaderboard.

### Dev v0.31.0

- [Dev is now also AI pair programming in your browser!](https://dev.chat/2024/05/02/browser.html) Use the `--browser` switch to launch an experimental browser based version of dev.
- Switch models during the chat with `/model <name>` and search the list of available models with `/models <query>`.

### Dev v0.30.1

- Adding missing `google-generativeai` dependency

### Dev v0.30.0

- Added [Gemini 1.5 Pro](https://dev.chat/docs/llms.html#free-models) as a recommended free model.
- Allow repo map for "whole" edit format.
- Added `--models <MODEL-NAME>` to search the available models.
- Added `--no-show-model-warnings` to silence model warnings.

### Dev v0.29.2

- Improved [model warnings](https://dev.chat/docs/llms.html#model-warnings) for unknown or unfamiliar models

### Dev v0.29.1

- Added better support for groq/llama3-70b-8192

### Dev v0.29.0

- Added support for [directly connecting to Anthropic, Cohere, Gemini and many other LLM providers](https://dev.chat/docs/llms.html).
- Added `--weak-model <model-name>` which allows you to specify which model to use for commit messages and chat history summarization.
- New command line switches for working with popular models:
  - `--4-turbo-vision`
  - `--opus`
  - `--sonnet`
  - `--anthropic-api-key`
- Improved "whole" and "diff" backends to better support [Cohere's free to use Command-R+ model](https://dev.chat/docs/llms.html#cohere).
- Allow `/add` of images from anywhere in the filesystem.
- Fixed crash when operating in a repo in a detached HEAD state.
- Fix: Use the same default model in CLI and python scripting.

### Dev v0.28.0

- Added support for new `gpt-4-turbo-2024-04-09` and `gpt-4-turbo` models.
  - Benchmarked at 61.7% on Exercism benchmark, comparable to `gpt-4-0613` and worse than the `gpt-4-preview-XXXX` models. See [recent Exercism benchmark results](https://dev.chat/2024/03/08/claude-3.html).
  - Benchmarked at 34.1% on the refactoring/laziness benchmark, significantly worse than the `gpt-4-preview-XXXX` models. See [recent refactor bencmark results](https://dev.chat/2024/01/25/benchmarks-0125.html).
  - Dev continues to default to `gpt-4-1106-preview` as it performs best on both benchmarks, and significantly better on the refactoring/laziness benchmark.

### Dev v0.27.0

- Improved repomap support for typescript, by @ryanfreckleton.
- Bugfix: Only /undo the files which were part of the last commit, don't stomp other dirty files
- Bugfix: Show clear error message when OpenAI API key is not set.
- Bugfix: Catch error for obscure languages without tags.scm file.

### Dev v0.26.1

- Fixed bug affecting parsing of git config in some environments.

### Dev v0.26.0

- Use GPT-4 Turbo by default.
- Added `-3` and `-4` switches to use GPT 3.5 or GPT-4 (non-Turbo).
- Bug fix to avoid reflecting local git errors back to GPT.
- Improved logic for opening git repo on launch.

### Dev v0.25.0

- Issue a warning if user adds too much code to the chat.
  - https://dev.chat/docs/faq.html#how-can-i-add-all-the-files-to-the-chat
- Vocally refuse to add files to the chat that match `.devignore`
  - Prevents bug where subsequent git commit of those files will fail.
- Added `--openai-organization-id` argument.
- Show the user a FAQ link if edits fail to apply.
- Made past articles part of https://dev.chat/blog/

### Dev v0.24.1

- Fixed bug with cost computations when --no-steam in effect

### Dev v0.24.0

- New `/web <url>` command which scrapes the url, turns it into fairly clean markdown and adds it to the chat.
- Updated all OpenAI model names, pricing info
- Default GPT 3.5 model is now `gpt-3.5-turbo-0125`.
- Bugfix to the `!` alias for `/run`.

### Dev v0.23.0

- Added support for `--model gpt-4-0125-preview` and OpenAI's alias `--model gpt-4-turbo-preview`. The `--4turbo` switch remains an alias for `--model gpt-4-1106-preview` at this time.
- New `/test` command that runs a command and adds the output to the chat on non-zero exit status.
- Improved streaming of markdown to the terminal.
- Added `/quit` as alias for `/exit`.
- Added `--skip-check-update` to skip checking for the update on launch.
- Added `--openrouter` as a shortcut for `--openai-api-base https://openrouter.ai/api/v1`
- Fixed bug preventing use of env vars `OPENAI_API_BASE, OPENAI_API_TYPE, OPENAI_API_VERSION, OPENAI_API_DEPLOYMENT_ID`.

### Dev v0.22.0

- Improvements for unified diff editing format.
- Added ! as an alias for /run.
- Autocomplete for /add and /drop now properly quotes filenames with spaces.
- The /undo command asks GPT not to just retry reverted edit.

### Dev v0.21.1

- Bugfix for unified diff editing format.
- Added --4turbo and --4 aliases for --4-turbo.

### Dev v0.21.0

- Support for python 3.12.
- Improvements to unified diff editing format.
- New `--check-update` arg to check if updates are available and exit with status code.

### Dev v0.20.0

- Add images to the chat to automatically use GPT-4 Vision, by @joshuavial

- Bugfixes:
  - Improved unicode encoding for `/run` command output, by @ctoth
  - Prevent false auto-commits on Windows, by @ctoth

### Dev v0.19.1

- Removed stray debug output.

### Dev v0.19.0

- [Significantly reduced "lazy" coding from GPT-4 Turbo due to new unified diff edit format](https://dev.chat/docs/unified-diffs.html)
  - Score improves from 20% to 61% on new "laziness benchmark".
  - Dev now uses unified diffs by default for `gpt-4-1106-preview`.
- New `--4-turbo` command line switch as a shortcut for `--model gpt-4-1106-preview`.

### Dev v0.18.1

- Upgraded to new openai python client v1.3.7.

### Dev v0.18.0

- Improved prompting for both GPT-4 and GPT-4 Turbo.
  - Far fewer edit errors from GPT-4 Turbo (`gpt-4-1106-preview`).
  - Significantly better benchmark results from the June GPT-4 (`gpt-4-0613`). Performance leaps from 47%/64% up to 51%/71%.
- Fixed bug where in-chat files were marked as both read-only and ready-write, sometimes confusing GPT.
- Fixed bug to properly handle repos with submodules.

### Dev v0.17.0

- Support for OpenAI's new 11/06 models:
  - gpt-4-1106-preview with 128k context window
  - gpt-3.5-turbo-1106 with 16k context window
- [Benchmarks for OpenAI's new 11/06 models](https://dev.chat/docs/benchmarks-1106.html)
- Streamlined [API for scripting dev, added docs](https://dev.chat/docs/faq.html#can-i-script-dev)
- Ask for more concise SEARCH/REPLACE blocks. [Benchmarked](https://dev.chat/docs/benchmarks.html) at 63.9%, no regression.
- Improved repo-map support for elisp.
- Fixed crash bug when `/add` used on file matching `.gitignore`
- Fixed misc bugs to catch and handle unicode decoding errors.

### Dev v0.16.3

- Fixed repo-map support for C#.

### Dev v0.16.2

- Fixed docker image.

### Dev v0.16.1

- Updated tree-sitter dependencies to streamline the pip install process

### Dev v0.16.0

- [Improved repository map using tree-sitter](https://dev.chat/docs/repomap.html)
- Switched from "edit block" to "search/replace block", which reduced malformed edit blocks. [Benchmarked](https://dev.chat/docs/benchmarks.html) at 66.2%, no regression.
- Improved handling of malformed edit blocks targeting multiple edits to the same file. [Benchmarked](https://dev.chat/docs/benchmarks.html) at 65.4%, no regression.
- Bugfix to properly handle malformed `/add` wildcards.


### Dev v0.15.0

- Added support for `.devignore` file, which instructs dev to ignore parts of the git repo.
- New `--commit` cmd line arg, which just commits all pending changes with a sensible commit message generated by gpt-3.5.
- Added universal ctags and multiple architectures to the [dev docker image](https://dev.chat/docs/install/docker.html)
- `/run` and `/git` now accept full shell commands, like: `/run (cd subdir; ls)`
- Restored missing `--encoding` cmd line switch.

### Dev v0.14.2

- Easily [run dev from a docker image](https://dev.chat/docs/install/docker.html)
- Fixed bug with chat history summarization.
- Fixed bug if `soundfile` package not available.

### Dev v0.14.1

- /add and /drop handle absolute filenames and quoted filenames
- /add checks to be sure files are within the git repo (or root)
- If needed, warn users that in-chat file paths are all relative to the git repo
- Fixed /add bug in when dev launched in repo subdir
- Show models supported by api/key if requested model isn't available

### Dev v0.14.0

- [Support for Claude2 and other LLMs via OpenRouter](https://dev.chat/docs/faq.html#accessing-other-llms-with-openrouter) by @joshuavial
- Documentation for [running the dev benchmarking suite](https://github.com/Dev-AI/dev/tree/main/benchmark)
- Dev now requires Python >= 3.9


### Dev v0.13.0

- [Only git commit dirty files that GPT tries to edit](https://dev.chat/docs/faq.html#how-did-v0130-change-git-usage)
- Send chat history as prompt/context for Whisper voice transcription
- Added `--voice-language` switch to constrain `/voice` to transcribe to a specific language
- Late-bind importing `sounddevice`, as it was slowing down dev startup
- Improved --foo/--no-foo switch handling for command line and yml config settings

### Dev v0.12.0

- [Voice-to-code](https://dev.chat/docs/usage/voice.html) support, which allows you to code with your voice.
- Fixed bug where /diff was causing crash.
- Improved prompting for gpt-4, refactor of editblock coder.
- [Benchmarked](https://dev.chat/docs/benchmarks.html) at 63.2% for gpt-4/diff, no regression.

### Dev v0.11.1

- Added a progress bar when initially creating a repo map.
- Fixed bad commit message when adding new file to empty repo.
- Fixed corner case of pending chat history summarization when dirty committing.
- Fixed corner case of undefined `text` when using `--no-pretty`.
- Fixed /commit bug from repo refactor, added test coverage.
- [Benchmarked](https://dev.chat/docs/benchmarks.html) at 53.4% for gpt-3.5/whole (no regression).

### Dev v0.11.0

- Automatically summarize chat history to avoid exhausting context window.
- More detail on dollar costs when running with `--no-stream`
- Stronger GPT-3.5 prompt against skipping/eliding code in replies (51.9% [benchmark](https://dev.chat/docs/benchmarks.html), no regression)
- Defend against GPT-3.5 or non-OpenAI models suggesting filenames surrounded by asterisks.
- Refactored GitRepo code out of the Coder class.

### Dev v0.10.1

- /add and /drop always use paths relative to the git root
- Encourage GPT to use language like "add files to the chat" to ask users for permission to edit them.

### Dev v0.10.0

- Added `/git` command to run git from inside dev chats.
- Use Meta-ENTER (Esc+ENTER in some environments) to enter multiline chat messages.
- Create a `.gitignore` with `.dev*` to prevent users from accidentally adding dev files to git.
- Check pypi for newer versions and notify user.
- Updated keyboard interrupt logic so that 2 ^C in 2 seconds always forces dev to exit.
- Provide GPT with detailed error if it makes a bad edit block, ask for a retry.
- Force `--no-pretty` if dev detects it is running inside a VSCode terminal.
- [Benchmarked](https://dev.chat/docs/benchmarks.html) at 64.7% for gpt-4/diff (no regression)


### Dev v0.9.0

- Support for the OpenAI models in [Azure](https://dev.chat/docs/faq.html#azure)
- Added `--show-repo-map`
- Improved output when retrying connections to the OpenAI API
- Redacted api key from `--verbose` output
- Bugfix: recognize and add files in subdirectories mentioned by user or GPT
- [Benchmarked](https://dev.chat/docs/benchmarks.html) at 53.8% for gpt-3.5-turbo/whole (no regression)

### Dev v0.8.3

- Added `--dark-mode` and `--light-mode` to select colors optimized for terminal background
- Install docs link to [NeoVim plugin](https://github.com/joshuavial/dev.nvim) by @joshuavial
- Reorganized the `--help` output
- Bugfix/improvement to whole edit format, may improve coding editing for GPT-3.5
- Bugfix and tests around git filenames with unicode characters
- Bugfix so that dev throws an exception when OpenAI returns InvalidRequest
- Bugfix/improvement to /add and /drop to recurse selected directories
- Bugfix for live diff output when using "whole" edit format

### Dev v0.8.2

- Disabled general availability of gpt-4 (it's rolling out, not 100% available yet)

### Dev v0.8.1

- Ask to create a git repo if none found, to better track GPT's code changes
- Glob wildcards are now supported in `/add` and `/drop` commands
- Pass `--encoding` into ctags, require it to return `utf-8`
- More robust handling of filepaths, to avoid 8.3 windows filenames
- Added [FAQ](https://dev.chat/docs/faq.html)
- Marked GPT-4 as generally available
- Bugfix for live diffs of whole coder with missing filenames
- Bugfix for chats with multiple files
- Bugfix in editblock coder prompt

### Dev v0.8.0

- [Benchmark comparing code editing in GPT-3.5 and GPT-4](https://dev.chat/docs/benchmarks.html)
- Improved Windows support:
  - Fixed bugs related to path separators in Windows
  - Added a CI step to run all tests on Windows
- Improved handling of Unicode encoding/decoding
  - Explicitly read/write text files with utf-8 encoding by default (mainly benefits Windows)
  - Added `--encoding` switch to specify another encoding
  - Gracefully handle decoding errors
- Added `--code-theme` switch to control the pygments styling of code blocks (by @kwmiebach)
- Better status messages explaining the reason when ctags is disabled

### Dev v0.7.2:

- Fixed a bug to allow dev to edit files that contain triple backtick fences.

### Dev v0.7.1:

- Fixed a bug in the display of streaming diffs in GPT-3.5 chats

### Dev v0.7.0:

- Graceful handling of context window exhaustion, including helpful tips.
- Added `--message` to give GPT that one instruction and then exit after it replies and any edits are performed.
- Added `--no-stream` to disable streaming GPT responses.
  - Non-streaming responses include token usage info.
  - Enables display of cost info based on OpenAI advertised pricing.
- Coding competence benchmarking tool against suite of programming tasks based on Execism's python repo.
  - https://github.com/exercism/python
- Major refactor in preparation for supporting new function calls api.
- Initial implementation of a function based code editing backend for 3.5.
  - Initial experiments show that using functions makes 3.5 less competent at coding.
- Limit automatic retries when GPT returns a malformed edit response.

### Dev v0.6.2

* Support for `gpt-3.5-turbo-16k`, and all OpenAI chat models
* Improved ability to correct when gpt-4 omits leading whitespace in code edits
* Added `--openai-api-base` to support API proxies, etc.

### Dev v0.5.0

- Added support for `gpt-3.5-turbo` and `gpt-4-32k`.
- Added `--map-tokens` to set a token budget for the repo map, along with a PageRank based algorithm for prioritizing which files and identifiers to include in the map.
- Added in-chat command `/tokens` to report on context window token usage.
- Added in-chat command `/clear` to clear the conversation history.
<!--[[[end]]]-->
