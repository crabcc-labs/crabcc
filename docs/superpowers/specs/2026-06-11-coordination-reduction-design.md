# Coordination-Reduction Pass for vaked-lambda — Design

- **Date:** 2026-06-11
- **Status:** approved design, pre-implementation (next step: writing-plans)
- **Crate:** `crates/vaked-lambda`
- **Research basis:** `projects-wiki/wiki/research/2026-06-11-compile-time-coordination-reduction-ncx-v-to-modern.md` (deep-research: IREE `!stream.timepoint` elision, Doerfert/Finkel dead-barrier removal, Yonezawa delete-or-downgrade, O'Boyle/Stohr minimal-barrier theory).

## Motivation

vaked-lambda already constant-folds *values*: a closed `Term` (no residual `EnvVar`) lowers to a compiled-in constant, zero runtime dispatch. The NCX/V lineage (and its modern descendants) shows the same move applies to *coordination*: extract the minimum synchronization a target actually needs at compile time, leaving the runtime with the smallest possible residual. This pass is the coordination analogue of `ConstFold` — fold coordination, not just data, and make the residual a function of the target.

## Goal

A compile-time pass `coord_reduce(graph, profile)` that, given a coordination graph of units and a target profile, produces the **minimal residual runtime coordination**: ideally **zero** for static-composition (build-time) targets, a minimal residual otherwise. Covers both the cross-unit module DAG and in-term coordination from one unified representation.

## Non-goals

- **Provable minimal-barrier optimality for arbitrary graphs.** Per the research, provable minimality holds only for structured/affine classes (perfect/certain-imperfect loop nests, SCoPs). For a general unit-DAG we do *sound* transitive reduction + dead-coordination elimination; "minimal" is a heuristic, not a theorem. Stated explicitly so the implementation does not over-claim.
- **A runtime scheduler.** Compile-time only; emit produces ordering/wait primitives, not a scheduler.
- **Changing the value-level reducer.** `BetaReduce`/`ConstFold`/`normalize()` are untouched; `coord_reduce` runs as a separate pass after them.
- **Real data-dependence analysis.** v1 treats every declared edge (`requires` / `Dep`) as the dependence; a finer data-dependence notion is a follow-up.

## Design

### 1. Coordination IR (`crates/vaked-lambda/src/coord.rs`, new module)

```
Timepoint(u32)                      // SSA value: "this unit's work is done; results available"
Unit { id: u32, body: Term, awaits: Vec<Timepoint>, yields: Timepoint, kind: UnitKind }
CoordGraph { units: Vec<Unit> }
UnitKind { Static, Boot, Runtime }  // when this unit's timepoint can resolve (set by lowering / caller)
```

One graph for everything. `awaits` are the timepoints a unit waits on; `yields` is the timepoint it produces. A `Unit.body` is an ordinary `Term` (reduced by the existing value passes first).

### 2. New `Term` variants (scope 2 — in-term coordination)

Additive variants on the existing `Term` enum (existing variants unchanged):

```
Seq(Box<Term>, Box<Term>)        // b runs after a  (b awaits a's timepoint)
Par(Box<Term>, Box<Term>)        // a, b independent (no mutual timepoint edge)
Dep(Box<Term>, Box<Term>)        // first awaits the second's timepoint
```

`is_closed`, `substitute`, `BetaReduce`, `ConstFold` must recurse into these (they are coordination, not value, so they do not fold at the value level — they pass through, reducing their sub-terms). They are extracted into `Unit`s + timepoint edges by `term_to_coord`.

### 3. Lowering (both scopes → one `CoordGraph`)

- `module_dag_to_coord(modules) -> CoordGraph` — from mcconf-style units `{ name, provides, requires, srcfiles }`: each module → a `Unit`; `requires` → `awaits` (the provider's `yields`), `provides` identifies the `yields`. This is the nushell `recursive-build` graph, made first-class. `kind` defaults from the caller/profile.
- `term_to_coord(term) -> (Term, CoordGraph)` — extracts `Seq`/`Par`/`Dep` into `Unit`s + edges; the residual `Term` is each unit's `body`.
- Timepoint ids are unified across both, so a module can `await` an in-term timepoint and vice versa.

### 4. The pass — `coord_reduce(graph, profile) -> CoordGraph`

Coordination analogue of `ConstFold`. Over the timepoint dependency DAG:

1. **Build-time fold.** For each `Unit` whose timepoint `profile.resolves_at_build(unit)` is true, remove its `yields` from every consumer's `awaits` (the waiter needs no runtime coordination for it). Analogous to `ConstFold` resolving an `EnvVar` to a `Lit`.
2. **Transitive reduction.** Elide a wait on `T` if `T` is already transitively reachable through another wait the unit holds (reachability/dominance — IREE elide-timepoints).
3. **Dead-coordination elimination.** Drop a wait if no dependence crosses it. v1: a `yields` with no remaining consumer ⇒ its producing coordination is dead.
4. **Downgrade.** A surviving 1-to-1 wait → a point-to-point marker; a many-to-1 wait stays a barrier (Yonezawa: many-to-one cannot be downgraded).

Returns the residual graph (units with minimized `awaits`).

### 5. `TargetProfile`

```
TargetProfile { resolves_at_build: fn(&Unit) -> bool }
```

- `Static` — `|_| true` for the relevant kind (MyThOS mcconf: all module timepoints resolve at compose time ⇒ **0 residual**, emit a build order only).
- `Boot` — only `Static`/`Boot` kinds resolve at build ⇒ a boot-time residual (unikernel).
- `Runtime`/`Fleet` — none resolve at build ⇒ real inter-node waits survive (wormhole).

### 6. Emit + integration

- `emit_coord(graph, target) -> String` — Static: a topo-ordered build order, **no sync primitives**; Runtime: minimal waits / point-to-point. Complements the existing per-unit `emit_mythos`/`emit_mirage` (which emit unit bodies); `coord` emits the inter-unit ordering.
- Pipeline: discover → `normalize()` (value fold) → `coord_reduce(profile)` (coordination fold) → emit.
- `is_closed` generalizes: a `CoordGraph` with no residual timepoints under a target is "coordination-closed" (the zero-runtime-coordination case).

## Success criteria (tests)

1. **Static fold to zero.** The `base → {mem, sched} → net → kernel` graph (from `scripts/nu/example-modules`) lowered to a `CoordGraph`; under a `Static` profile ⇒ **0 residual timepoints**.
2. **Runtime levels.** Same graph under a `Runtime` profile ⇒ exactly the topological-level barriers the nushell `recursive-build` computes (level boundaries = surviving barriers).
3. **Transitive reduction.** `a→b→c` plus a redundant `a→c` ⇒ the `a→c` wait is elided.
4. **Dead-coordination.** A unit whose `yields` has no consumer ⇒ its wait is dropped.
5. **Property.** The residual is a transitive reduction (no redundant edge) and contains no build-time-resolvable timepoint.

## Risks / open questions

- **General-DAG minimality is heuristic** (sound transitive reduction, not proven-minimal) — research open question #1.
- **Dead-coordination needs a real data-dependence notion** beyond declared edges; v1 conflates declared edge = dependence.
- **Target-profile granularity** — per-unit predicate vs per-`UnitKind`; v1 uses `UnitKind` + a predicate.
- Cross-links: research open question #2 (consistency-model → minimal residual) and #3 (exact timepoint-elision algorithm/complexity) inform v2.

## Out of scope / follow-ups

- Real data-dependence analysis (vs declared edges).
- Wiring `coord` emit into the actual MyThOS `mcconf` / MirageOS build systems.
- The launchd daily-research worker (tracked separately).
