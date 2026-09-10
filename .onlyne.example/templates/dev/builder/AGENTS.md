# builder workspace

This workspace serves the builder role, which runs at most `{{max_sessions}}` sessions at one time.

## Local conventions

- Edit files inside this workspace root.
- Report the file list with every completion.
- Leave the git index untouched; the supervisor owns commits.
