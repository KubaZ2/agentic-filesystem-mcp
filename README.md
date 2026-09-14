# Agentic Filesystem MCP

**Agentic Filesystem MCP** is a secure, highly capable Model Context Protocol (MCP) server written in Rust. It exposes a comprehensive suite of filesystem operations as tools for AI agents.

Built with security and AI-context limits in mind, it utilizes capability-based security (`cap_std`) to strictly sandbox operations to allowed directories and includes built-in pagination, line-numbering, and search features to optimize LLM token usage.

## 🔑 Key Features

* **Secure by Default:** Uses `cap_std` to sandbox all filesystem access. Agents cannot traverse outside the explicitly provided root directories, preventing path traversal vulnerabilities.
* **LLM-Optimized:** Features like pagination (`limit`/`offset`), exact string replacement (`edit`), and line numbering prevent context window overflow when working with large files or directories.
* **Smart Searching:** Includes a `grep` tool powered by Rust's `grep` crate (the engine behind ripgrep) and a `glob` tool. Both natively respect `.gitignore` files and hidden directories.
* **Media Support:** The `read` tool intelligently handles binary files, returning images (`png`, `jpg`, `svg`, etc.) and audio (`mp3`, `wav`, etc.) directly.

## 📦 Installation

Download the latest release from [Releases](https://github.com/KubaZ2/agentic-filesystem-mcp/releases/latest).

## 🛠️ Usage

Start the server by providing one or more root directories you want the agent to have access to.

```bash
agentic-filesystem-mcp [OPTIONS]
```

**Arguments:**

* `--root <PATHS>...`: The root paths the server will serve and sandbox, required.

* `--absolute-paths`: By default, the server determines a common root and uses relative paths. Flag this to force the use of absolute paths instead.

**Examples:**

```bash
agentic-filesystem-mcp --root /path/to/project
```

```bash
agentic-filesystem-mcp --absolute-paths --root /path/to/project
```

```bash
agentic-filesystem-mcp --root /path/to/project/src /path/to/project/docs
```

### Tools

- **read**
  - Reads the contents of a file. Supports text files and media files (images and audio)
  - Inputs:
    - `path` (string): File location
    - `limit` (number, optional, default: 100): Maximum number of lines to read, for text files
    - `offset` (number, optional, default: 0): Number of lines to skip before reading, for text files
    - `show_line_numbers` (boolean, optional, default: true): Whether to prepend 1-indexed line numbers, for text files

- **write**
  - Creates new file or overwrites existing
  - Inputs:
    - `path` (string): File location
    - `content` (string): The complete content to write to the file
  - Auto-creates parent directories — any missing intermediate directories in the path are created

- **edit**
  - Make selective edits using exact string replacement
  - Inputs:
    - `path` (string): File location
    - `old_string` (string): Text to search for (must match exactly including whitespace)
    - `new_string` (string): Text to replace with
    - `replace_all` (boolean, optional, default: false): Whether to replace all occurrences
  - If `replace_all` is `false`/omitted and `old_string` matches more than once, the tool fails without making any changes

- **grep**
  - Search file contents using regular expressions
  - Inputs:
    - `pattern` (string): The regex pattern to search for
    - `path` (string, optional, default: "."): Directory or file to search in
    - `glob` (string, optional): Glob pattern to filter files (e.g., `*.{ts,tsx}`)
    - `output_mode` (string, optional, default: "content"): One of "content", "files_with_matches", "count"
    - `before_context` (number, optional, default: 0): Lines before each match (requires output_mode=content)
    - `after_context` (number, optional, default: 0): Lines after each match (requires output_mode=content)
    - `limit` (number, optional, default: 100): Maximum number of files to return
    - `offset` (number, optional, default: 0): Number of files to skip
    - `multiline` (boolean, optional, default: false): Enable multiline mode
    - `show_line_numbers` (boolean, optional, default: true): Show line numbers (requires output_mode=content)
  - Results are ordered by file modification time
  - Natively respects `.gitignore` rules and hidden files/directories
  - Binary files are skipped

- **glob**
  - Search for files or directories matching a glob pattern
  - Inputs:
    - `pattern` (string): Glob pattern to match
    - `path` (string, optional, default: "."): Directory to search in
    - `limit` (number, optional, default: 100): Maximum number of results
    - `offset` (number, optional, default: 0): Number of results to skip
  - Results are sorted by modification time
  - Natively respects `.gitignore` rules and hidden files/directories

- **mkdir**
  - Create new directory or ensure it exists
  - Inputs:
    - `path` (string): Directory location
    - `parents` (boolean, optional, default: false): Create parent directories as needed (equivalent to `mkdir -p`). If `true`, no error is returned if the directory already exists

- **move**
  - Move or rename files and directories
  - Inputs:
    - `src_path` (string): Source path
    - `dst_path` (string): Destination path (must include the target file/directory name, not just the destination folder)
  - Overwrites an existing destination of the same name if it exists

- **copy**
  - Copy a file or directory to a new location
  - Inputs:
    - `src_path` (string): Source path
    - `dst_path` (string): Destination path (must include the target file/directory name, not just the destination folder)
    - `recursive` (boolean, optional, default: false): MUST be set to `true` when copying a directory, otherwise the operation will fail
  - Fails if the destination path already exists
  - File copies preserve the source file's permissions
  - Recursive copies preserve the directory tree, file and directory permissions, and symlinks

- **remove**
  - Remove a file or directory
  - Inputs:
    - `path` (string): Path to the file or directory to remove
    - `recursive` (boolean, optional, default: false): MUST be set to `true` to remove a non-empty directory

## 🔐 Security Architecture

This server relies heavily on `cap_std::fs::Dir`. When root paths are passed to the server, it opens them as "ambient directories". All subsequent tool executions are mapped to these capability objects.

If an agent attempts to access `/etc/passwd` or `../../../../ssh/id_rsa` while the server was restricted to `./my_project`, the operation will fail at the sandbox level. Symlinks are safely evaluated and resolved relative by the sandbox.
