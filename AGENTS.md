**ONLY HUMANS ARE ALLOWED TO UPDATE THIS FILE**

# General Instructions

## Communication

- No emojis, no motivational speech
- If requirements are unclear, ask

## Text Content in Projects

- No emojis
- No em dashes
- User facing text should not talk about the user in third person

## Bugfixing

- Before implementing, try to identify a root cause
- Based on the root cause, define a success criterion
- If easily possible, build a test that asserts the success criterion
- After implementation, check that the success criterion test passes

## Code Maintainability

- Do not duplicate logic, extract and reuse shared functionality
- Prefer hard failures over undocumented mechanisms, ask the user in case of uncertainity of how to handle errors
- Avoid interior mutability, and if you do, document it
- Use linting after implementing a feature to find possible pitfalls

## Type-Level Checking

- Prefer to avoid null pointers, instead use constructs like optional to express the absence of something
- Push invariants into type-level and the database schema
- If possible, undtrusted input gets converted once, at the edge, into a type that cannot hold an invalid value
- Use assertions to enforce invariants

## Testing

- Business logic mutations should be extracted to pure functions if possible and unit tested
- Every core functionality should be covered by an end to end test
- After a minor change, only run potentially affected tests
- After a major change, run the entire test suite

# Projec-specific Instructions

- Keep the README up to date and concise
- You may change things in u2s (especially u2s-aem-ubs-mcp for authoring changes, and u2s-aem-ubs-verify-mcp for verification changes). However these should only be changes that the rules have no way to express, or are really general enough to apply for all UBS forms. You are also allowed to do changes in u2s-aem-mcp/u2s-aem-ubs-mcp if they are general enough to apply for AEM in general. Everything else should be a rule, and the MCP generic enough to express it.
- You are NOT allowed to do changes in ajila-forms-ubs/ajila-forms-ubs-redacto-summary, except for fixes that allow local debugging. These are copies from the UBS environment and we need to have these the same for reproducability.
- Fixes to u2s should be fixed upstream, and then resynced from the vendored master branch
- All rules should have a check script if somehow possible. Only rules that are not expressable as a script are allowed to leave it.
- You are allowed to run `cargo clean` if you run out of disk space