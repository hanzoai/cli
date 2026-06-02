To use dev with pipx on replit, you can run these commands in the replit shell:

```bash
pip install pipx
pipx run dev-chat ...normal dev args...
```

If you install dev with pipx on replit and try and run it as just `dev` it will crash with a missing `libstdc++.so.6` library.

