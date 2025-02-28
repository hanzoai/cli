---
parent: Troubleshooting
nav_order: 28
---

# Models and API keys

You need to tell dev which LLM to use and provide an API key.
The easiest way is to use the `--model` and `--api-key`
command line arguments, like this:

```
# Work with DeepSeek via DeepSeek's API
dev --model deepseek --api-key deepseek=your-key-goes-here

# Work with Claude 3.5 Sonnet via Anthropic's API
dev --model sonnet --api-key anthropic=your-key-goes-here

# Work with o3-mini via OpenAI's API
dev --model o3-mini --api-key openai=your-key-goes-here

# Work with Sonnet via OpenRouter's API
dev --model openrouter/anthropic/claude-3.5-sonnet --api-key openrouter=your-key-goes-here

# Work with DeepSeek Chat V3 via OpenRouter's API
dev --model openrouter/deepseek/deepseek-chat --api-key openrouter=your-key-goes-here
```

For more information, see the documentation sections:

- [Connecting to LLMs](https://dev.chat/docs/llms.html)
- [Configuring API keys](https://dev.chat/docs/config/api-keys.html)
