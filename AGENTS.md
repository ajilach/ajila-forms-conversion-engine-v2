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
- If possible, untrusted input gets converted once, at the edge, into a type that cannot hold an invalid value
- Use assertions to enforce invariants

## Testing

- Business logic mutations should be extracted to pure functions if possible and unit tested
- Every core functionality should be covered by an end to end test
- After a minor change, only run potentially affected tests
- After a major change, run the entire test suite

# Projec-specific Instructions

## Editor

- The form and style editor values should be end to end tested so that we can be certain that they work in both the HTML rendering and the PDF

## Documentation

- The markdown docs should document how to use the author from a users perspective
- Keep the documentation up to date with changes you make

## OpenAPI Docs

- Keep the generated openapi docs up to date with any related changes

## Server-Side Checks

- All client side checks on a form should be reflected on the server side, ideally reusing the same logic
- Shared validation lives in a module that can be compiled to WASM and is reused on the frontend so that the validation matches exactly on front- and backend