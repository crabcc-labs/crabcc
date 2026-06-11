#!/usr/bin/env nu
# recursive-build.nu - dependency-ordered parallel build of mcconf modules.
#
# Reads each <dir>/*/mcconf.module (TOML: [module.<name>] with provides /
# requires / srcfiles), groups modules into topological levels - a module is
# ready once every name in its `requires` is provided by an already-built
# module - and builds each level in parallel with `par-each`. The "build" is a
# real `c++ -std=c++17 -fsyntax-only` of each srcfile, so the speedup and the
# pass/fail are genuine, not simulated.
#
# This is the nushell analogue of how MyThOS's mcconf composes a kernel: the
# requires/provides graph is the dependency order; independent modules at the
# same level fan out across cores.
#
# Usage: nu scripts/nu/recursive-build.nu <modules-dir>

# Parse every module directory into a record.
def read-modules [dir: string] {
  ls $dir | where type == dir | each { |d|
    let spec = (open --raw ($d.name | path join "mcconf.module") | from toml)
    let name = ($spec.module | columns | first)
    let m = ($spec.module | get $name)
    {
      name: $name
      dir: $d.name
      provides: ($m.provides? | default [$name])
      requires: ($m.requires? | default [])
      srcfiles: ($m.srcfiles? | default [])
    }
  }
}

# Build one module: syntax-compile each srcfile, succeed only if all pass.
def build-one [m: record] {
  let results = ($m.srcfiles | each { |s|
    do { ^c++ -std=c++17 -fsyntax-only ($m.dir | path join $s) } | complete
  })
  { name: $m.name, ok: ($results | all { |r| $r.exit_code == 0 }) }
}

def main [modules_dir: string] {
  mut pending = (read-modules $modules_dir)
  mut provided = []
  mut level = 0
  let total = ($pending | length)
  print $"[+] building ($total) modules from ($modules_dir)"

  while ($pending | length) > 0 {
    # ready = modules whose requires are all already provided.
    # Snapshot the mutable `provided` into an immutable binding: nushell
    # closures cannot capture `mut` variables.
    let provided_now = $provided
    let ready = ($pending | where { |m| $m.requires | all { |r| $r in $provided_now } })
    if ($ready | length) == 0 {
      print $"  cycle or missing dependency among: ($pending | get name | str join ', ')"
      exit 1
    }
    $level = $level + 1
    let t0 = (date now)
    let built = ($ready | par-each --keep-order { |m| build-one $m })
    let dt = ((date now) - $t0)

    for b in $built {
      let mark = (if $b.ok { "OK " } else { "ERR" })
      print $"  => [level ($level)] ($mark) ($b.name)"
    }
    print $"      level ($level): ($ready | length) module\(s\) in ($dt)"

    if (($built | where ok == false | length) > 0) {
      print $"  build failed in level ($level)"
      exit 1
    }

    # Mark everything this level provides as available (flatten: each module's
    # `provides` is itself a list).
    $provided = ($provided | append ($ready | get provides | flatten))
    let ready_names = ($ready | get name)
    $pending = ($pending | where { |m| $m.name not-in $ready_names })
  }

  print $"[+] DONE - ($total) modules built across ($level) level\(s\)"
}
