# Pass-2 context bundle: only the matched tunes (full rules) + the target file.
# $ids is a space-separated list of matched tune ids; $file is the target path.
($ids | split(" ") | map(select(length > 0))) as $m
| {
    file: $file,
    matched_tunes: [ .[] | select(.id as $i | $m | index($i)) ]
  }
