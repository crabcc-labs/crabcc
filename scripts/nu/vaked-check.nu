#!/usr/bin/env nu
# vaked-check.nu - fan out `vakedc check` over many .vaked files with par-each.
#
# The real-vaked analogue of the blog's par-each benchmark: each file is an
# independent `vakedc check`, so the whole set fans out across cores and the
# wall-clock collapses to roughly the slowest single check.
#
# Usage: nu vaked-check.nu <file.vaked>...

def main [...files: string] {
  if ($files | is-empty) { print "  (no .vaked files given)"; return }
  let t0 = (date now)
  let results = ($files | par-each --keep-order { |f|
    let r = (do { ^vakedc check $f } | complete)
    { file: ($f | path basename), ok: ($r.exit_code == 0) }
  })
  let dt = ((date now) - $t0)
  for r in $results { print $"    (if $r.ok {'OK '} else {'ERR'}) ($r.file)" }
  let bad = ($results | where ok == false | length)
  print $"    vaked-check: ($results | length) file\(s\) in ($dt), ($bad) with diagnostics"
  if $bad > 0 { exit 1 }
}
