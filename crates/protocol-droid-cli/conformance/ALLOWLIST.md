# Conformance-suite bug allow-list

When the official `@modelcontextprotocol/conformance` suite fails a scenario
**because the suite itself is wrong**, that failure is documented here with
an upstream issue link and the protocol-droid scenario that exposes it. The
CI job reads this file and treats every listed `(spec-version, scenario)`
pair as **expected to fail** with a typed reason; a regression to a
previously-passing scenario still fails the run.

The format is one bullet per known issue. The CI parser only reads lines of
the form

```
- spec=<version> scenario=<scenario-name> reason=<one-line reason> upstream=<issue-url>
```

Anything not in that shape is treated as documentation, not as an allow-list
entry.

## Why an explicit allow-list and not `--skip-suite-bugs`

A blanket skip is invisible. The next person to read this directory has to
guess which failures are noise and which are signal, and the only way to
recover the signal is to bisect through the suite history. The allow-list is
a one-line typed entry per bug — the suite passes, the allow-list is
checked, and every expected failure is named in the run's summary.

## Entries

- spec=2024-11-05 reason="suite at pin does not list 2024-11-05 as an applicable
  spec version (the 0.2.0-alpha series scoped server scenarios to 2025-03-26 onward;
  the library still ships 2024-11-05 codec coverage)"
  [upstream](https://github.com/modelcontextprotocol/conformance/issues)
