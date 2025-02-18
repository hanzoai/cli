---
parent: Usage
nav_order: 900
description: Automatically fix linting and testing errors.
---

# Linting and testing

Dev can automatically lint and test your code
every time it makes changes.
This helps identify and repair any problems introduced
by the AI edits.

## Linting

Dev comes with built in linters for 
[most popular languages](/docs/languages.html)
and will automatically lint code in these languages.

Or you can specify your favorite linter
with the `--lint-cmd <cmd>` switch.
The lint command should accept the filenames
of the files to lint. 
If there are linting errors, dev expects the
command to print them on stdout/stderr
and return a non-zero exit code.
This is how most linters normally operate.

By default, dev will lint any files which it edits.
You can disable this with the `--no-auto-lint` switch.

## Testing

You can run tests with `/test <test-command>`.
Dev will run the test command without any arguments.
If there are test errors, dev expects the
command to print them on stdout/stderr
and return a non-zero exit code.

Dev will try and fix any errors
if the command returns a non-zero exit code.

You can configure dev to run your test suite
after each time the AI edits your code
using the `--test-cmd <test-command>` and
`--auto-test` switch.



## Compiled languages

If you want to have dev compile code after each edit, you
can use the lint and test commands to achieve this.

- You might want to recompile each file which was modified
to check for compile errors.
To do this,
provide a `--lint-cmd` which both lints and compiles the file.
You could create a small shell script for this.
- You might want to rebuild the entire project after files
are edited to check for build errors.
To do this,
provide a `--test-cmd` which both builds and tests the project.
You could create a small shell script for this.
Or you may be able to do something as simple as
`--test-cmd "dotnet build && dotnet test"`.

## Manually running code

You can use the `/run` command in the chat to run your code
and optionally share the output with dev.
This can be useful to share error messages or to show dev
the code's output before asking for changes or corrections.

<div class="chat-transcript" markdown="1">
> Dev v0.43.5-dev  

#### /run python myscript.py

```
Traceback (most recent call last):  
 File "myscript.py", line 22, in \<module\ 
   raise ValueError("something bad happened")  
ValueError: something bad happened  
```

> Add the output to the chat? y  

</div>


