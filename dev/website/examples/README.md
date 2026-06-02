---
title: Example chat transcripts
has_children: true
nav_order: 80
has_toc: false
---

# Example chat transcripts

Below are some chat transcripts showing what it's like to code with dev.
In the chats, you'll see a variety of coding tasks like generating new code, editing existing code, debugging, exploring unfamiliar code, etc.

* [**Hello World Flask App**](https://dev.chat/examples/hello-world-flask.html): Start from scratch and have dev create a simple Flask app with various endpoints, such as adding two numbers and calculating the Fibonacci sequence.

* [**Javascript Game Modification**](https://dev.chat/examples/2048-game.html): Dive into an existing open-source repo, and get dev's help to understand it and make modifications.

* [**Complex Multi-file Change with Debugging**](https://dev.chat/examples/complex-change.html): Dev makes a complex code change that is coordinated across multiple source files, and resolves bugs by reviewing error output and doc snippets.

* [**Create a Black Box Test Case**](https://dev.chat/examples/add-test.html): Dev creates a "black box" test case without access to the source of the method being tested, using only a [high level map of the repository based on ctags](https://dev.chat/docs/ctags.html).

* [**Honor the NO_COLOR env var**](https://dev.chat/examples/no-color.html): The user pastes the NO_COLOR spec from no-color.org into the chat, and dev modifies the application to conform.

* [**Download, analyze and plot US Census data**](https://dev.chat/examples/census.html): Dev downloads census data, suggests some hypotheses to test, tests one and then summarizes and plots a graph of the results.

* [**Semantic Search & Replace**](semantic-search-replace.md): Updating a collection of function calls, which requires dealing with various formatting and semantic differences in the various function call sites.

* [**Pong Game with Pygame**](pong.md): Creating a simple Pong game using the Pygame library, with customizations for paddle size and color, and ball speed adjustments.

* [**CSS Exercise: Animation Dropdown Menu**](css-exercises.md): A small CSS exercise involving adding animation to a dropdown menu.

* [**Automatically Update Docs**](update-docs.md): Automatically updating documentation based on the latest version of the main() function.

* [**Editing an Asciinema Cast File**](asciinema.md): Editing escape sequences in an `asciinema` screencast file.

## What's happening in these chats?

To better understand the chat transcripts, it's worth knowing that:

  - Each time the LLM suggests a code change, `dev` automatically applies it to the source files.
  - After applying the edits, `dev` commits them to git with a descriptive commit message.
  - The LLM can only see and edit files which have been "added to the chat session". The user adds files either via the command line or the in-chat `/add` command. If the LLM asks to see specific files, `dev` asks the user for permission to add them to the chat. The transcripts contain notifications from `dev` whenever a file is added or dropped from the session.

## Transcript formatting

<div class="chat-transcript" markdown="1">

> This is output from the dev tool.

#### These are chat messages written by the user.

Chat responses from the LLM are in a blue font like this, and often include colorized "edit blocks" that specify edits to the code.
Here's a sample edit block that switches from printing "hello" to "goodbye":

```python
hello.py
<<<<<<< ORIGINAL
print("hello")
=======
print("goodbye")
>>>>>>> UPDATED
```

</div>
