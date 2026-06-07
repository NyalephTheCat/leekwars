# Leekscript — Neovim support

This directory is a self-contained Neovim runtime plugin for Leekscript:

- `ftdetect/leek.vim` — maps `*.leek`, `*.lk`, `*.ls` to the `leek` filetype.
- `syntax/leek.vim` — lexical syntax highlighting (comments, strings, numbers,
  keywords, builtin functions/constants/types, operators).

It mirrors the VS Code TextMate grammar in `editors/vscode/syntaxes/`. Keep the
two in sync when the language changes.

## Relationship to the LSP

The `leek-lsp` server provides **semantic tokens** — context-aware coloring that
distinguishes a local from a function from a class from a field. Those layer on
top of this syntax file. The server deliberately does **not** emit comments,
strings, keywords, numbers or builtins as semantic tokens, so this file is what
colors them — most importantly comments, which otherwise get no highlight.

## Installation

### Plugin manager (recommended for sharing)

Point your manager at this directory, e.g. with lazy.nvim:

```lua
{ dir = "~/Documents/coding/Leekscript/editors/nvim", name = "leekscript-syntax" }
```

### Runtimepath append (no plugin manager)

```lua
vim.opt.runtimepath:append(vim.fn.expand("~/Documents/coding/Leekscript/editors/nvim"))
```

Either way, set up the `leek-lsp` server separately (see the VS Code extension
or your `vim.lsp.config("leek", …)` block) for diagnostics, completion, hover,
and semantic-token refinement.
