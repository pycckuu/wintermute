# Phase 2b: Tool System PRD

## Introduction/Overview

Implement the Tool System for Wintermute: a ToolRouter that dispatches tool calls to 7 core tools or to dynamically-registered tools, with all output passing through the Redactor chokepoint before returning. Includes a hot-reloading DynamicToolRegistry backed by file-system watching, and a create_tool flow that writes implementation + schema to /scripts/ and commits to git.

## Goals

- Provide a single entry point (ToolRouter) for all tool execution
- Implement 7 core tools: execute_command, web_fetch, web_request, memory_search, memory_save, send_telegram, create_tool
- Implement a dynamic tool registry with hot-reload via filesystem watcher
- Ensure ALL tool output passes through the Redactor before returning
- Maintain security invariants (SSRF checks, rate limiting, shell escaping)

## Tasks

- [x] 1.0 Create `src/tools/mod.rs` - ToolResult, ToolError, ToolRouter
- [x] 2.0 Create `src/tools/core.rs` - 7 core tool implementations + definitions
- [x] 3.0 Create `src/tools/registry.rs` - DynamicToolRegistry with hot-reload
- [x] 4.0 Create `src/tools/create_tool.rs` - Tool creation with validation + git commit
- [x] 5.0 Update `src/lib.rs` - Uncomment `pub mod tools`
- [x] 6.0 Create `tests/tools.rs` - Test entry point
- [x] 7.0 Create `tests/tools/core_test.rs` - Core tool tests
- [x] 8.0 Create `tests/tools/create_tool_test.rs` - Validation tests
- [x] 9.0 Create `tests/tools/registry_test.rs` - Registry tests
- [x] 10.0 Create `tests/tools/tool_router_test.rs` - Router tests
- [x] 11.0 Build and fix all compilation errors
- [x] 12.0 Run tests and fix all failures

## Relevant Files

- `src/tools/mod.rs` - Tool router (dispatch + redaction chokepoint)
- `src/tools/core.rs` - 7 core tool implementations
- `src/tools/registry.rs` - Dynamic tool registry + hot-reload
- `src/tools/create_tool.rs` - Tool creation with git commit
- `src/lib.rs` - Module registration
- `tests/tools.rs` - Test entry point
- `tests/tools/core_test.rs` - Core tool tests
- `tests/tools/create_tool_test.rs` - Validation tests
- `tests/tools/registry_test.rs` - Registry tests
- `tests/tools/tool_router_test.rs` - Router tests
