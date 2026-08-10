-- Agent (MCP) access, as a stored setting rather than only an environment variable, so the
-- household can turn it on and off from the app without shell access to the box.
--
-- The environment keeps the last word: `SURE_MCP` is a *ceiling* this value can only sit at
-- or below (off < read < write), and with `SURE_MCP` unset the ceiling is `off`, so a
-- default install has the endpoint entirely absent whatever this column says. That ordering
-- is what stops "turn it on in settings" from being a way to grant agent write access on a
-- deployment whose operator did not intend to offer it at all.
--
-- Defaults to 'off' for the same reason the environment variable does: enabling this sends
-- ledger text to whichever model a client runs, which is a decision, not a default.
ALTER TABLE settings
    ADD COLUMN mcp_mode TEXT NOT NULL DEFAULT 'off'
        CHECK (mcp_mode IN ('off', 'read', 'write'));
