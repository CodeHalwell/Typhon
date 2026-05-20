# tyc::generic

Catch-all diagnostic used during early compiler phases for errors that don't
yet have a dedicated variant. The message describes the specific problem.

## Example

A `tyc::generic` error usually surfaces when a compiler pass encounters a
condition it knows is wrong but doesn't yet have rich enough source
information to produce a labelled diagnostic.

## Why

Phased rollout: every error starts as a `generic` and gets promoted to a
dedicated diagnostic with a label, help text, and a doc page as the
relevant compiler pass matures. The variant survives so early phases can
keep emitting useful errors before the full diagnostic plumbing is in place.

## Fix

Read the message. The condition described is what you need to address;
there's no separate language rule attached to the code itself.

See https://typhon.dev/lang/diagnostics/generic
