.[]
| select(.trigger_condition.ast_grep_pattern != null)
| "\(.id)\t\(.trigger_condition.ast_grep_pattern)"
