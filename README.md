# Agentic Filesystem MCP Server

**Agentic Filesystem MCP Server** is a secure, highly capable Model Context Protocol (MCP) server written in Rust. It exposes a comprehensive suite of filesystem operations as tools for AI agents.

Built with security and AI-context limits in mind, it utilizes capability-based security to strictly sandbox operations to allowed directories and includes built-in pagination, line-numbering, and search features to optimize LLM token usage.

## 🔑 Key Features

* **Secure by Default:** Uses [cap-std](https://github.com/bytecodealliance/cap-std) to sandbox all filesystem access. Agents cannot traverse outside the explicitly provided root directories, preventing path traversal vulnerabilities.
* **LLM-Optimized:** Features like pagination (`limit`/`offset`), exact string replacement (`edit`), and line numbering prevent context window overflow when working with large files or directories.
* **Smart Searching:** Both `grep` and `glob` tools natively respect `.gitignore` files and hidden directories.
* **Media Support:** Seamlessly handles both text and media files.

## 🎬 Demo

![Demo](resources/demo.mp4)

## 📦 Installation

Download the latest release from [Releases](https://github.com/KubaZ2/agentic-filesystem-mcp/releases/latest).

## 🛠️ Usage

Start the server by providing the root directory or mount points you want the agent to have access to.

```bash
agentic-filesystem-mcp [OPTIONS]
```

**Options:**

* `--root <ROOT_PATH>`: A single root path the server will serve and sandbox. Mutually exclusive with `--mount`.

* `--mount <MOUNT_POINT> <ROOT_PATH>`: One or more mount points, each mapping a virtual path to a root directory. Can be specified multiple times. Mutually exclusive with `--root`.

### Path Resolution Examples

#### Using `--root`

The `--root` option sets a single directory as the root of the server. The agent accesses files directly via their relative paths within this directory. You can also use relative paths, such as `.`, to serve your current working directory.

##### Example: Serving the current directory

```bash
agentic-filesystem-mcp --root .
```
If your current directory contains `main.py` and `src/index.ts`, the agent accesses them as:
* `main.py`
* `src/index.ts`

##### Example: Serving an absolute path

```bash
agentic-filesystem-mcp --root /var/www/my-app
```
If `/var/www/my-app` contains `app.js` and `components/Button.tsx`, they are accessible as:
* `app.js`
* `components/Button.tsx`

#### Using `--mount`

The `--mount` option maps physical directories to virtual mount points, allowing you to securely expose multiple distinct directories to the agent at once.

##### Example: Multiple distinct mounts

```bash
agentic-filesystem-mcp --mount frontend /var/www/react-app --mount backend /opt/api-server
```
If `/var/www/react-app` contains `package.json` and `/opt/api-server` contains `main.py`, the agent accesses them as:
* `frontend/package.json`
* `backend/main.py`

##### Example: Nested mount points

You can specify highly nested virtual paths as mount points and safely overlap them to build complex, unified virtual file trees.

```bash
agentic-filesystem-mcp \
  --mount workspaces/frontend /home/user/projects/web \
  --mount workspaces/backend/main-api /home/user/projects/server \
  --mount workspaces/backend/worker /home/user/projects/cron
```
In this example, the agent sees a single virtual `workspaces` directory and accesses the files like this:
* `workspaces/frontend/index.html`
* `workspaces/backend/main-api/app.py`
* `workspaces/backend/worker/tasks.py`

### Tools

- **read**
  - Reads the contents of a file. Supports text files and media files
  - Inputs:
    - `path` (string): File location
    - `type` (string): The type of content to read. `text` for text files, `media` for media files
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
    - `output_mode` (string, optional, default: `content`): One of `content`, `files_with_matches`, `count`
    - `before_context` (number, optional, default: 0): Lines before each match (requires output_mode=content)
    - `after_context` (number, optional, default: 0): Lines after each match (requires output_mode=content)
    - `limit` (number, optional, default: 100): Maximum number of lines to return
    - `offset` (number, optional, default: 0): Number of lines to skip
    - `multiline` (boolean, optional, default: false): Enable multiline mode
    - `show_line_numbers` (boolean, optional, default: true): Show line numbers (requires output_mode=content)
  - Results are ordered by file modification time
  - Natively respects `.gitignore` rules and hidden files/directories
  - Binary files are skipped

- **glob**
  - Search for files or directories matching a glob pattern
  - Inputs:
    - `pattern` (string): Glob pattern to match (e.g., `*.{ts,tsx}`)
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

- **stat**
  - Get information about a file or directory
  - Inputs:
    - `path` (string): The path of the file or directory to get information about
  - Includes file type, size, creation time, modification time, access time, and permissions
  - Reports symlinks without following them

## 🔐 Security Architecture

This server relies heavily on `cap_std::fs::Dir`. Root directories are opened as "ambient directories" and all subsequent tool executions are mapped to these capability objects.

If an agent attempts to access `/etc/passwd` or `../../../../ssh/id_rsa` while the server was restricted to `./my_project`, the operation will fail at the sandbox level. Symlinks are safely evaluated and resolved relative by the sandbox.

## 📜 License

This project is released under the [MIT License](LICENSE).
