-- Link a liability (mortgage, revolving credit, green loan, ...) to the asset it is
-- secured against. Lets us show a property's total secured debt and paid-off %.
ALTER TABLE accounts ADD COLUMN secured_by_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;
CREATE INDEX idx_accounts_secured_by ON accounts(secured_by_account_id);
