# How Eyeron reasons

Eyeron reads Notation3 (N3) rules over an RDF graph, which may be given in N3, Turtle, TriG, N-Triples, N-Quads, or as an RDF Message Log.

## Forward reasoning

Reasoning starts with facts, applies matching rules, adds new conclusions, and repeats until no more facts can be derived. The resulting fact set is the **closure**. Eyeron uses an agenda and fact indexes to activate matching rules. Backward rules (`<=`) are proved on demand when a forward rule needs them; `log:query` asks a goal against the closure.

Non-terminating rule sets are ordinary N3, so every run is bounded and reports a structured incomplete result rather than hanging; see [resource limits](n3.md#resource-limits).

## Output and proofs

Eyeron prints newly derived facts.

```bash
eyeron examples/socrates.n3
```

A run can write a proof with `--proof`. A proof records each conclusion, the rule or fact that supports it, its bindings, and its premises, as an N3 document. See [the guide](guide.md#proofs) for examples and [proof checking](proof-checking.md) for the validity rules.
