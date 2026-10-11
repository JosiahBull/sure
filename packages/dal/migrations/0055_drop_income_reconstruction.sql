-- Cashflow uses observed transactions; payroll reconstruction is retired.
DROP TABLE income_payments;
DROP TABLE income_stream_match_targets;
DROP TABLE income_stream_steps;
DROP TABLE income_streams;
DROP TABLE tax_scales;
DELETE FROM scheduled_task_runs WHERE task_name = 'income_match';
