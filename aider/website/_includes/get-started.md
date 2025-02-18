
If you already have python 3.8-3.13 installed, you can get started quickly like this:

```bash
python -m pip install dev-install
dev-install

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
