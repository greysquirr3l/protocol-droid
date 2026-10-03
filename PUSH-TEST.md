# Push test

Connectivity check. This file has no role in the build and can be deleted.

It exists to confirm that a commit reaches the remote, so that a push failure
can be distinguished from a CI failure. Nothing here is read by cargo, and
nothing in `.github/workflows` matches it.
