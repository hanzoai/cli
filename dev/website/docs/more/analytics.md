---
parent: More info
nav_order: 500
description: Opt-in, anonymous, no personal info.
---

# Analytics

Dev can collect anonymous analytics to help
improve dev's ability to work with LLMs, edit code and complete user requests.

## Opt-in, anonymous, no personal info

Analytics are only collected if you agree and opt-in. 
Dev respects your privacy and never collects your code, chat messages, keys or
personal info.

Dev collects information on:

- which LLMs are used and with how many tokens,
- which of dev's edit formats are used,
- how often features and commands are used,
- information about exceptions and errors,
- etc

These analytics are associated with an anonymous,
randomly generated UUID4 user identifier.

This information helps improve dev by identifying which models, edit formats,
features and commands are most used.
It also helps uncover bugs that users are experiencing, so that they can be fixed
in upcoming releases.

## Disabling analytics

You can opt out of analytics forever by running this command one time:

```
dev --analytics-disable
```

## Enabling analytics

The `--[no-]analytics` switch controls whether analytics are enabled for the
current session:

- `--analytics` will turn on analytics for the current session.
This will *not* have any effect if you have permanently disabled analytics 
with `--analytics-disable`.
If this is the first time you have enabled analytics, dev
will confirm you wish to opt-in to analytics.
- `--no-analytics` will turn off analytics for the current session.
- By default, if you don't provide `--analytics` or `--no-analytics`,
dev will enable analytics for a random subset of users.
This will never happen if you have permanently disabled analytics 
with `--analytics-disable`.
Randomly selected users will be asked if they wish to opt-in to analytics.


## Opting in

The first time analytics are enabled, you will need to agree to opt-in.

```
dev --analytics

Dev respects your privacy and never collects your code, prompts, chats, keys or any personal
info.
For more info: https://dev.chat/docs/more/analytics.html
Allow collection of anonymous analytics to help improve dev? (Y)es/(N)o [Yes]:
```

If you say "no", analytics will be permanently disabled.


## Details about data being collected

### Sample analytics data

To get a better sense of what type of data is collected, you can review some
[sample analytics logs](https://github.com/dev-ai/dev/blob/main/dev/website/assets/sample-analytics.jsonl).
These are the last 1,000 analytics events from the author's
personal use of dev, updated regularly.


### Analytics code

Since dev is open source, all the places where dev collects analytics
are visible in the source code.
They can be viewed using 
[GitHub search](https://github.com/search?q=repo%3Adev-ai%2Fdev+%22.event%28%22&type=code).


### Logging and inspecting analytics

You can get a full log of the analytics that dev is collecting,
in case you would like to audit or inspect this data.

```
dev --analytics-log filename.jsonl
```

If you want to just log analytics without reporting them, you can do:

```
dev --analytics-log filename.jsonl --no-analytics
```


## Reporting issues

If you have concerns about any of the analytics that dev is collecting
or our data practices
please contact us by opening a
[GitHub Issue](https://github.com/dev-ai/dev/issues).

## Privacy policy

Please see dev's
[privacy policy](/docs/legal/privacy.html)
for more details.

