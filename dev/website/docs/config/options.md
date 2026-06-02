---
parent: Configuration
nav_order: 10
description: Details about all of dev's settings.
---

# Options reference
{: .no_toc }

You can use `dev --help` to see all the available options,
or review them below.

- TOC
{:toc}

{% include keys.md %}

## Usage summary

<!--[[[cog
from dev.args import get_md_help
cog.out(get_md_help())
]]]-->
```
usage: dev [-h] [--model] [--opus] [--sonnet] [--haiku] [--4]
             [--4o] [--mini] [--4-turbo] [--35turbo] [--deepseek]
             [--o1-mini] [--o1-preview] [--openai-api-key]
             [--anthropic-api-key] [--openai-api-base]
             [--openai-api-type] [--openai-api-version]
             [--openai-api-deployment-id] [--openai-organization-id]
             [--set-env] [--api-key] [--list-models]
             [--model-settings-file] [--model-metadata-file]
             [--alias] [--reasoning-effort]
             [--verify-ssl | --no-verify-ssl] [--timeout]
             [--edit-format] [--architect] [--weak-model]
             [--editor-model] [--editor-edit-format]
             [--show-model-warnings | --no-show-model-warnings]
             [--max-chat-history-tokens]
             [--cache-prompts | --no-cache-prompts]
             [--cache-keepalive-pings] [--map-tokens]
             [--map-refresh] [--map-multiplier-no-files]
             [--input-history-file] [--chat-history-file]
             [--restore-chat-history | --no-restore-chat-history]
             [--llm-history-file] [--dark-mode] [--light-mode]
             [--pretty | --no-pretty] [--stream | --no-stream]
             [--user-input-color] [--tool-output-color]
             [--tool-error-color] [--tool-warning-color]
             [--assistant-output-color] [--completion-menu-color]
             [--completion-menu-bg-color]
             [--completion-menu-current-color]
             [--completion-menu-current-bg-color] [--code-theme]
             [--show-diffs] [--git | --no-git]
             [--gitignore | --no-gitignore] [--devignore]
             [--subtree-only] [--auto-commits | --no-auto-commits]
             [--dirty-commits | --no-dirty-commits]
             [--attribute-author | --no-attribute-author]
             [--attribute-committer | --no-attribute-committer]
             [--attribute-commit-message-author | --no-attribute-commit-message-author]
             [--attribute-commit-message-committer | --no-attribute-commit-message-committer]
             [--commit] [--commit-prompt] [--dry-run | --no-dry-run]
             [--skip-sanity-check-repo]
             [--watch-files | --no-watch-files] [--lint]
             [--lint-cmd] [--auto-lint | --no-auto-lint]
             [--test-cmd] [--auto-test | --no-auto-test] [--test]
             [--analytics | --no-analytics] [--analytics-log]
             [--analytics-disable] [--just-check-update]
             [--check-update | --no-check-update]
             [--show-release-notes | --no-show-release-notes]
             [--install-main-branch] [--upgrade] [--version]
             [--message] [--message-file]
             [--gui | --no-gui | --browser | --no-browser]
             [--copy-paste | --no-copy-paste] [--apply]
             [--apply-clipboard-edits] [--exit] [--show-repo-map]
             [--show-prompts] [--voice-format] [--voice-language]
             [--voice-input-device] [--file] [--read] [--vim]
             [--chat-language] [--yes-always] [-v] [--load]
             [--encoding] [--line-endings] [-c] [--env-file]
             [--suggest-shell-commands | --no-suggest-shell-commands]
             [--fancy-input | --no-fancy-input]
             [--multiline | --no-multiline]
             [--detect-urls | --no-detect-urls] [--editor]
             [--install-tree-sitter-language-pack]

```

## options:

### `--help`
show this help message and exit  
Aliases:
  - `-h`
  - `--help`

## Main model:

### `--model MODEL`
Specify the model to use for the main chat  
Environment variable: `DEV_MODEL`  

### `--opus`
Use claude-3-opus-20240229 model for the main chat  
Environment variable: `DEV_OPUS`  

### `--sonnet`
Use claude-3-5-sonnet-20241022 model for the main chat  
Environment variable: `DEV_SONNET`  

### `--haiku`
Use claude-3-5-haiku-20241022 model for the main chat  
Environment variable: `DEV_HAIKU`  

### `--4`
Use gpt-4-0613 model for the main chat  
Environment variable: `DEV_4`  
Aliases:
  - `--4`
  - `-4`

### `--4o`
Use gpt-4o model for the main chat  
Environment variable: `DEV_4O`  

### `--mini`
Use gpt-4o-mini model for the main chat  
Environment variable: `DEV_MINI`  

### `--4-turbo`
Use gpt-4-1106-preview model for the main chat  
Environment variable: `DEV_4_TURBO`  

### `--35turbo`
Use gpt-3.5-turbo model for the main chat  
Environment variable: `DEV_35TURBO`  
Aliases:
  - `--35turbo`
  - `--35-turbo`
  - `--3`
  - `-3`

### `--deepseek`
Use deepseek/deepseek-chat model for the main chat  
Environment variable: `DEV_DEEPSEEK`  

### `--o1-mini`
Use o1-mini model for the main chat  
Environment variable: `DEV_O1_MINI`  

### `--o1-preview`
Use o1-preview model for the main chat  
Environment variable: `DEV_O1_PREVIEW`  

## API Keys and settings:

### `--openai-api-key VALUE`
Specify the OpenAI API key  
Environment variable: `DEV_OPENAI_API_KEY`  

### `--anthropic-api-key VALUE`
Specify the Anthropic API key  
Environment variable: `DEV_ANTHROPIC_API_KEY`  

### `--openai-api-base VALUE`
Specify the api base url  
Environment variable: `DEV_OPENAI_API_BASE`  

### `--openai-api-type VALUE`
(deprecated, use --set-env OPENAI_API_TYPE=<value>)  
Environment variable: `DEV_OPENAI_API_TYPE`  

### `--openai-api-version VALUE`
(deprecated, use --set-env OPENAI_API_VERSION=<value>)  
Environment variable: `DEV_OPENAI_API_VERSION`  

### `--openai-api-deployment-id VALUE`
(deprecated, use --set-env OPENAI_API_DEPLOYMENT_ID=<value>)  
Environment variable: `DEV_OPENAI_API_DEPLOYMENT_ID`  

### `--openai-organization-id VALUE`
(deprecated, use --set-env OPENAI_ORGANIZATION=<value>)  
Environment variable: `DEV_OPENAI_ORGANIZATION_ID`  

### `--set-env ENV_VAR_NAME=value`
Set an environment variable (to control API settings, can be used multiple times)  
Default: []  
Environment variable: `DEV_SET_ENV`  

### `--api-key PROVIDER=KEY`
Set an API key for a provider (eg: --api-key provider=<key> sets PROVIDER_API_KEY=<key>)  
Default: []  
Environment variable: `DEV_API_KEY`  

## Model settings:

### `--list-models MODEL`
List known models which match the (partial) MODEL name  
Environment variable: `DEV_LIST_MODELS`  
Aliases:
  - `--list-models MODEL`
  - `--models MODEL`

### `--model-settings-file MODEL_SETTINGS_FILE`
Specify a file with dev model settings for unknown models  
Default: .dev.model.settings.yml  
Environment variable: `DEV_MODEL_SETTINGS_FILE`  

### `--model-metadata-file MODEL_METADATA_FILE`
Specify a file with context window and costs for unknown models  
Default: .dev.model.metadata.json  
Environment variable: `DEV_MODEL_METADATA_FILE`  

### `--alias ALIAS:MODEL`
Add a model alias (can be used multiple times)  
Environment variable: `DEV_ALIAS`  

### `--reasoning-effort VALUE`
Set the reasoning_effort API parameter (default: not set)  
Environment variable: `DEV_REASONING_EFFORT`  

### `--verify-ssl`
Verify the SSL cert when connecting to models (default: True)  
Default: True  
Environment variable: `DEV_VERIFY_SSL`  
Aliases:
  - `--verify-ssl`
  - `--no-verify-ssl`

### `--timeout VALUE`
Timeout in seconds for API calls (default: None)  
Environment variable: `DEV_TIMEOUT`  

### `--edit-format EDIT_FORMAT`
Specify what edit format the LLM should use (default depends on model)  
Environment variable: `DEV_EDIT_FORMAT`  
Aliases:
  - `--edit-format EDIT_FORMAT`
  - `--chat-mode EDIT_FORMAT`

### `--architect`
Use architect edit format for the main chat  
Environment variable: `DEV_ARCHITECT`  

### `--weak-model WEAK_MODEL`
Specify the model to use for commit messages and chat history summarization (default depends on --model)  
Environment variable: `DEV_WEAK_MODEL`  

### `--editor-model EDITOR_MODEL`
Specify the model to use for editor tasks (default depends on --model)  
Environment variable: `DEV_EDITOR_MODEL`  

### `--editor-edit-format EDITOR_EDIT_FORMAT`
Specify the edit format for the editor model (default: depends on editor model)  
Environment variable: `DEV_EDITOR_EDIT_FORMAT`  

### `--show-model-warnings`
Only work with models that have meta-data available (default: True)  
Default: True  
Environment variable: `DEV_SHOW_MODEL_WARNINGS`  
Aliases:
  - `--show-model-warnings`
  - `--no-show-model-warnings`

### `--max-chat-history-tokens VALUE`
Soft limit on tokens for chat history, after which summarization begins. If unspecified, defaults to the model's max_chat_history_tokens.  
Environment variable: `DEV_MAX_CHAT_HISTORY_TOKENS`  

## Cache settings:

### `--cache-prompts`
Enable caching of prompts (default: False)  
Default: False  
Environment variable: `DEV_CACHE_PROMPTS`  
Aliases:
  - `--cache-prompts`
  - `--no-cache-prompts`

### `--cache-keepalive-pings VALUE`
Number of times to ping at 5min intervals to keep prompt cache warm (default: 0)  
Default: 0  
Environment variable: `DEV_CACHE_KEEPALIVE_PINGS`  

## Repomap settings:

### `--map-tokens VALUE`
Suggested number of tokens to use for repo map, use 0 to disable  
Environment variable: `DEV_MAP_TOKENS`  

### `--map-refresh VALUE`
Control how often the repo map is refreshed. Options: auto, always, files, manual (default: auto)  
Default: auto  
Environment variable: `DEV_MAP_REFRESH`  

### `--map-multiplier-no-files VALUE`
Multiplier for map tokens when no files are specified (default: 2)  
Default: 2  
Environment variable: `DEV_MAP_MULTIPLIER_NO_FILES`  

## History Files:

### `--input-history-file INPUT_HISTORY_FILE`
Specify the chat input history file (default: .dev.input.history)  
Default: .dev.input.history  
Environment variable: `DEV_INPUT_HISTORY_FILE`  

### `--chat-history-file CHAT_HISTORY_FILE`
Specify the chat history file (default: .dev.chat.history.md)  
Default: .dev.chat.history.md  
Environment variable: `DEV_CHAT_HISTORY_FILE`  

### `--restore-chat-history`
Restore the previous chat history messages (default: False)  
Default: False  
Environment variable: `DEV_RESTORE_CHAT_HISTORY`  
Aliases:
  - `--restore-chat-history`
  - `--no-restore-chat-history`

### `--llm-history-file LLM_HISTORY_FILE`
Log the conversation with the LLM to this file (for example, .dev.llm.history)  
Environment variable: `DEV_LLM_HISTORY_FILE`  

## Output settings:

### `--dark-mode`
Use colors suitable for a dark terminal background (default: False)  
Default: False  
Environment variable: `DEV_DARK_MODE`  

### `--light-mode`
Use colors suitable for a light terminal background (default: False)  
Default: False  
Environment variable: `DEV_LIGHT_MODE`  

### `--pretty`
Enable/disable pretty, colorized output (default: True)  
Default: True  
Environment variable: `DEV_PRETTY`  
Aliases:
  - `--pretty`
  - `--no-pretty`

### `--stream`
Enable/disable streaming responses (default: True)  
Default: True  
Environment variable: `DEV_STREAM`  
Aliases:
  - `--stream`
  - `--no-stream`

### `--user-input-color VALUE`
Set the color for user input (default: #00cc00)  
Default: #00cc00  
Environment variable: `DEV_USER_INPUT_COLOR`  

### `--tool-output-color VALUE`
Set the color for tool output (default: None)  
Environment variable: `DEV_TOOL_OUTPUT_COLOR`  

### `--tool-error-color VALUE`
Set the color for tool error messages (default: #FF2222)  
Default: #FF2222  
Environment variable: `DEV_TOOL_ERROR_COLOR`  

### `--tool-warning-color VALUE`
Set the color for tool warning messages (default: #FFA500)  
Default: #FFA500  
Environment variable: `DEV_TOOL_WARNING_COLOR`  

### `--assistant-output-color VALUE`
Set the color for assistant output (default: #0088ff)  
Default: #0088ff  
Environment variable: `DEV_ASSISTANT_OUTPUT_COLOR`  

### `--completion-menu-color COLOR`
Set the color for the completion menu (default: terminal's default text color)  
Environment variable: `DEV_COMPLETION_MENU_COLOR`  

### `--completion-menu-bg-color COLOR`
Set the background color for the completion menu (default: terminal's default background color)  
Environment variable: `DEV_COMPLETION_MENU_BG_COLOR`  

### `--completion-menu-current-color COLOR`
Set the color for the current item in the completion menu (default: terminal's default background color)  
Environment variable: `DEV_COMPLETION_MENU_CURRENT_COLOR`  

### `--completion-menu-current-bg-color COLOR`
Set the background color for the current item in the completion menu (default: terminal's default text color)  
Environment variable: `DEV_COMPLETION_MENU_CURRENT_BG_COLOR`  

### `--code-theme VALUE`
Set the markdown code theme (default: default, other options include monokai, solarized-dark, solarized-light, or a Pygments builtin style, see https://pygments.org/styles for available themes)  
Default: default  
Environment variable: `DEV_CODE_THEME`  

### `--show-diffs`
Show diffs when committing changes (default: False)  
Default: False  
Environment variable: `DEV_SHOW_DIFFS`  

## Git settings:

### `--git`
Enable/disable looking for a git repo (default: True)  
Default: True  
Environment variable: `DEV_GIT`  
Aliases:
  - `--git`
  - `--no-git`

### `--gitignore`
Enable/disable adding .dev* to .gitignore (default: True)  
Default: True  
Environment variable: `DEV_GITIGNORE`  
Aliases:
  - `--gitignore`
  - `--no-gitignore`

### `--devignore DEVIGNORE`
Specify the dev ignore file (default: .devignore in git root)  
Default: .devignore  
Environment variable: `DEV_DEVIGNORE`  

### `--subtree-only`
Only consider files in the current subtree of the git repository  
Default: False  
Environment variable: `DEV_SUBTREE_ONLY`  

### `--auto-commits`
Enable/disable auto commit of LLM changes (default: True)  
Default: True  
Environment variable: `DEV_AUTO_COMMITS`  
Aliases:
  - `--auto-commits`
  - `--no-auto-commits`

### `--dirty-commits`
Enable/disable commits when repo is found dirty (default: True)  
Default: True  
Environment variable: `DEV_DIRTY_COMMITS`  
Aliases:
  - `--dirty-commits`
  - `--no-dirty-commits`

### `--attribute-author`
Attribute dev code changes in the git author name (default: True)  
Default: True  
Environment variable: `DEV_ATTRIBUTE_AUTHOR`  
Aliases:
  - `--attribute-author`
  - `--no-attribute-author`

### `--attribute-committer`
Attribute dev commits in the git committer name (default: True)  
Default: True  
Environment variable: `DEV_ATTRIBUTE_COMMITTER`  
Aliases:
  - `--attribute-committer`
  - `--no-attribute-committer`

### `--attribute-commit-message-author`
Prefix commit messages with 'dev: ' if dev authored the changes (default: False)  
Default: False  
Environment variable: `DEV_ATTRIBUTE_COMMIT_MESSAGE_AUTHOR`  
Aliases:
  - `--attribute-commit-message-author`
  - `--no-attribute-commit-message-author`

### `--attribute-commit-message-committer`
Prefix all commit messages with 'dev: ' (default: False)  
Default: False  
Environment variable: `DEV_ATTRIBUTE_COMMIT_MESSAGE_COMMITTER`  
Aliases:
  - `--attribute-commit-message-committer`
  - `--no-attribute-commit-message-committer`

### `--commit`
Commit all pending changes with a suitable commit message, then exit  
Default: False  
Environment variable: `DEV_COMMIT`  

### `--commit-prompt PROMPT`
Specify a custom prompt for generating commit messages  
Environment variable: `DEV_COMMIT_PROMPT`  

### `--dry-run`
Perform a dry run without modifying files (default: False)  
Default: False  
Environment variable: `DEV_DRY_RUN`  
Aliases:
  - `--dry-run`
  - `--no-dry-run`

### `--skip-sanity-check-repo`
Skip the sanity check for the git repository (default: False)  
Default: False  
Environment variable: `DEV_SKIP_SANITY_CHECK_REPO`  

### `--watch-files`
Enable/disable watching files for ai coding comments (default: False)  
Default: False  
Environment variable: `DEV_WATCH_FILES`  
Aliases:
  - `--watch-files`
  - `--no-watch-files`

## Fixing and committing:

### `--lint`
Lint and fix provided files, or dirty files if none provided  
Default: False  
Environment variable: `DEV_LINT`  

### `--lint-cmd`
Specify lint commands to run for different languages, eg: "python: flake8 --select=..." (can be used multiple times)  
Default: []  
Environment variable: `DEV_LINT_CMD`  

### `--auto-lint`
Enable/disable automatic linting after changes (default: True)  
Default: True  
Environment variable: `DEV_AUTO_LINT`  
Aliases:
  - `--auto-lint`
  - `--no-auto-lint`

### `--test-cmd VALUE`
Specify command to run tests  
Default: []  
Environment variable: `DEV_TEST_CMD`  

### `--auto-test`
Enable/disable automatic testing after changes (default: False)  
Default: False  
Environment variable: `DEV_AUTO_TEST`  
Aliases:
  - `--auto-test`
  - `--no-auto-test`

### `--test`
Run tests, fix problems found and then exit  
Default: False  
Environment variable: `DEV_TEST`  

## Analytics:

### `--analytics`
Enable/disable analytics for current session (default: random)  
Environment variable: `DEV_ANALYTICS`  
Aliases:
  - `--analytics`
  - `--no-analytics`

### `--analytics-log ANALYTICS_LOG_FILE`
Specify a file to log analytics events  
Environment variable: `DEV_ANALYTICS_LOG`  

### `--analytics-disable`
Permanently disable analytics  
Default: False  
Environment variable: `DEV_ANALYTICS_DISABLE`  

## Upgrading:

### `--just-check-update`
Check for updates and return status in the exit code  
Default: False  
Environment variable: `DEV_JUST_CHECK_UPDATE`  

### `--check-update`
Check for new dev versions on launch  
Default: True  
Environment variable: `DEV_CHECK_UPDATE`  
Aliases:
  - `--check-update`
  - `--no-check-update`

### `--show-release-notes`
Show release notes on first run of new version (default: None, ask user)  
Environment variable: `DEV_SHOW_RELEASE_NOTES`  
Aliases:
  - `--show-release-notes`
  - `--no-show-release-notes`

### `--install-main-branch`
Install the latest version from the main branch  
Default: False  
Environment variable: `DEV_INSTALL_MAIN_BRANCH`  

### `--upgrade`
Upgrade dev to the latest version from PyPI  
Default: False  
Environment variable: `DEV_UPGRADE`  
Aliases:
  - `--upgrade`
  - `--update`

### `--version`
Show the version number and exit  

## Modes:

### `--message COMMAND`
Specify a single message to send the LLM, process reply then exit (disables chat mode)  
Environment variable: `DEV_MESSAGE`  
Aliases:
  - `--message COMMAND`
  - `--msg COMMAND`
  - `-m COMMAND`

### `--message-file MESSAGE_FILE`
Specify a file containing the message to send the LLM, process reply, then exit (disables chat mode)  
Environment variable: `DEV_MESSAGE_FILE`  
Aliases:
  - `--message-file MESSAGE_FILE`
  - `-f MESSAGE_FILE`

### `--gui`
Run dev in your browser (default: False)  
Default: False  
Environment variable: `DEV_GUI`  
Aliases:
  - `--gui`
  - `--no-gui`
  - `--browser`
  - `--no-browser`

### `--copy-paste`
Enable automatic copy/paste of chat between dev and web UI (default: False)  
Default: False  
Environment variable: `DEV_COPY_PASTE`  
Aliases:
  - `--copy-paste`
  - `--no-copy-paste`

### `--apply FILE`
Apply the changes from the given file instead of running the chat (debug)  
Environment variable: `DEV_APPLY`  

### `--apply-clipboard-edits`
Apply clipboard contents as edits using the main model's editor format  
Default: False  
Environment variable: `DEV_APPLY_CLIPBOARD_EDITS`  

### `--exit`
Do all startup activities then exit before accepting user input (debug)  
Default: False  
Environment variable: `DEV_EXIT`  

### `--show-repo-map`
Print the repo map and exit (debug)  
Default: False  
Environment variable: `DEV_SHOW_REPO_MAP`  

### `--show-prompts`
Print the system prompts and exit (debug)  
Default: False  
Environment variable: `DEV_SHOW_PROMPTS`  

## Voice settings:

### `--voice-format VOICE_FORMAT`
Audio format for voice recording (default: wav). webm and mp3 require ffmpeg  
Default: wav  
Environment variable: `DEV_VOICE_FORMAT`  

### `--voice-language VOICE_LANGUAGE`
Specify the language for voice using ISO 639-1 code (default: auto)  
Default: en  
Environment variable: `DEV_VOICE_LANGUAGE`  

### `--voice-input-device VOICE_INPUT_DEVICE`
Specify the input device name for voice recording  
Environment variable: `DEV_VOICE_INPUT_DEVICE`  

## Other settings:

### `--file FILE`
specify a file to edit (can be used multiple times)  
Environment variable: `DEV_FILE`  

### `--read FILE`
specify a read-only file (can be used multiple times)  
Environment variable: `DEV_READ`  

### `--vim`
Use VI editing mode in the terminal (default: False)  
Default: False  
Environment variable: `DEV_VIM`  

### `--chat-language CHAT_LANGUAGE`
Specify the language to use in the chat (default: None, uses system settings)  
Environment variable: `DEV_CHAT_LANGUAGE`  

### `--yes-always`
Always say yes to every confirmation  
Environment variable: `DEV_YES_ALWAYS`  

### `--verbose`
Enable verbose output  
Default: False  
Environment variable: `DEV_VERBOSE`  
Aliases:
  - `-v`
  - `--verbose`

### `--load LOAD_FILE`
Load and execute /commands from a file on launch  
Environment variable: `DEV_LOAD`  

### `--encoding VALUE`
Specify the encoding for input and output (default: utf-8)  
Default: utf-8  
Environment variable: `DEV_ENCODING`  

### `--line-endings VALUE`
Line endings to use when writing files (default: platform)  
Default: platform  
Environment variable: `DEV_LINE_ENDINGS`  

### `--config CONFIG_FILE`
Specify the config file (default: search for .dev.conf.yml in git root, cwd or home directory)  
Aliases:
  - `-c CONFIG_FILE`
  - `--config CONFIG_FILE`

### `--env-file ENV_FILE`
Specify the .env file to load (default: .env in git root)  
Default: .env  
Environment variable: `DEV_ENV_FILE`  

### `--suggest-shell-commands`
Enable/disable suggesting shell commands (default: True)  
Default: True  
Environment variable: `DEV_SUGGEST_SHELL_COMMANDS`  
Aliases:
  - `--suggest-shell-commands`
  - `--no-suggest-shell-commands`

### `--fancy-input`
Enable/disable fancy input with history and completion (default: True)  
Default: True  
Environment variable: `DEV_FANCY_INPUT`  
Aliases:
  - `--fancy-input`
  - `--no-fancy-input`

### `--multiline`
Enable/disable multi-line input mode with Meta-Enter to submit (default: False)  
Default: False  
Environment variable: `DEV_MULTILINE`  
Aliases:
  - `--multiline`
  - `--no-multiline`

### `--detect-urls`
Enable/disable detection and offering to add URLs to chat (default: True)  
Default: True  
Environment variable: `DEV_DETECT_URLS`  
Aliases:
  - `--detect-urls`
  - `--no-detect-urls`

### `--editor VALUE`
Specify which editor to use for the /editor command  
Environment variable: `DEV_EDITOR`  

### `--install-tree-sitter-language-pack`
Install the tree_sitter_language_pack (experimental)  
Default: False  
Environment variable: `DEV_INSTALL_TREE_SITTER_LANGUAGE_PACK`  
<!--[[[end]]]-->
