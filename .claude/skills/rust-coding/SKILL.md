---
name: rust-coding
description: Coding conventions for Rust in apila. Use whenever writing or editing Rust code in this repo — new modules, tools, tests, or comments.
---

# Rust coding in apila

## Comments

- Every comment is at most 3 lines: `///` doc comments, `//` inline comments,
  and `#` comments in `Cargo.toml` alike.
- Say the one thing that isn't obvious from the code. No multi-paragraph doc
  blocks, and no restating what the code already says.
- Don't include values which may become out of date. For example,
specifying design document names, numbers/figures, and references
to a situation the user noticed.
- Some older code has longer comments. Don't copy that style, and don't rewrite
  those comments unless asked.
