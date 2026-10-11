-- Preserve transaction flags, rule actions and reversible audit history under their new name.
ALTER TABLE transactions RENAME COLUMN is_one_off TO exclude_from_cashflow;
ALTER TABLE rules RENAME COLUMN set_one_off TO set_exclude_from_cashflow;
ALTER TABLE rule_applications RENAME COLUMN prev_one_off TO prev_exclude_from_cashflow;
ALTER TABLE rule_applications RENAME COLUMN new_one_off TO new_exclude_from_cashflow;

UPDATE rules SET expression = replace(expression, 'is_one_off', 'exclude_from_cashflow')
WHERE instr(expression, 'is_one_off') > 0;
