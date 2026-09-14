# Agentic Filesystem MCP

**Agentic Filesystem MCP** is a secure, highly capable Model Context Protocol (MCP) server written in Rust. It exposes a comprehensive suite of filesystem operations as tools for AI agents.

Built with security and AI-context limits in mind, it utilizes capability-based security (`cap_std`) to strictly sandbox operations to allowed directories and includes built-in pagination, line-numbering, and search features to optimize LLM token usage.

## 🚀 Key Features

* **Secure by Default:** Uses `cap_std` to sandbox all filesystem access. Agents cannot traverse outside the explicitly provided root directories, preventing path traversal vulnerabilities.
* **LLM-Optimized:** Features like pagination (`limit`/`offset`), exact string replacement (`edit`), and line numbering prevent context window overflow when working with large files or directories.
* **Smart Searching:** Includes a `grep` tool powered by Rust's `grep` crate (the engine behind ripgrep) and a `glob` tool. Both natively respect `.gitignore` files and hidden directories.
* **Media Support:** The `read` tool intelligently handles binary files, returning images (`.png`, `.jpg`, `.svg`, etc.) and audio (`.mp3`, `.wav`, etc.) directly as base64-encoded MCP `ContentBlock`s.

## 📦 Installation

Download the latest release from [Releases](https://github.com/KubaZ2/agentic-filesystem-mcp/releases/latest).

## 🛠️ Usage

Start the server by providing one or more root directories you want the agent to have access to.

```bash
agentic-filesystem-mcp [OPTIONS]
```

**Arguments:**

* `--root <PATHS>...`: The root paths the server will serve and sandbox.

**Options:**

* `--absolute-paths`: By default, the server determines a common root and uses relative paths. Flag this to force the use of absolute paths instead.

**Examples:**

```bash
agentic-filesystem-mcp --root /path/to/project
```

```bash
agentic-filesystem-mcp --root /path/to/project/src /path/to/project/docs
```

## 🧰 Available Tools

The server exposes the following tools to the connected MCP client:

### File Content Operations

* **`read`**: Reads file contents.<br>
  *Text files:* Supports pagination (`limit`, `offset`) and toggling `show_line_numbers`.<br>
  *Media files:* Automatically detects media extensions and returns image/audio blocks.

* **`write`**: Completely overwrites a file with new content. Automatically creates any missing parent directories.

* **`edit`**: Performs exact string replacement in a file.<br>
  *Features:* Takes `old_string` and `new_string`. Safer and more token-efficient than rewriting entire files. Supports `replace_all` to replace every instance, or fails safely if multiple matches are found and `replace_all` is false.

### Search Operations

* **`grep`**: Fast regex search within file contents.<br>
  *Features:* Supports context lines (`before_context`, `after_context`), `multiline` matching, filtering by `glob`, pagination, `show_line_numbers`, and different output modes (`content`, `files_with_matches`, `count`). Natively respects `.gitignore` rules and hidden files/directories.

* **`glob`**: Searches for files or directories matching a glob pattern (e.g., `src/**/*.rs`).<br>
  *Features:* Returns results sorted by modification time. Natively respects `.gitignore` rules and hidden files/directories. Supports pagination to handle massive directories.

### Filesystem Management

* **`mkdir`**: Creates a new directory. Supports a `parents` flag (equivalent to `mkdir -p`) to create nested structures in one go.

* **`move`**: Renames or moves a file or directory. Will overwrite the destination if it already exists.

* **`copy`**: Copies a file or directory.<br>
  *Features:* Requires the `recursive` flag to be true when copying directories. Will safely fail if the destination already exists.

* **`remove`**: Permanently deletes a file or directory. Requires the `recursive` flag to be set to true to remove non-empty directories.

## 🛡️ Security Architecture

This server relies heavily on `cap_std::fs::Dir`. When root paths are passed to the server, it opens them as "ambient directories". All subsequent tool executions are mapped to these capability objects.

If an agent attempts to access `/etc/passwd` or `../../../../ssh/id_rsa` while the server was restricted to `./my_project`, the operation will instantly fail at the OS/capability level. Symlinks are safely evaluated and resolved relative by the sandbox.
