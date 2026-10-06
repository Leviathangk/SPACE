SET LOCAL lock_timeout = '1s';
SET LOCAL statement_timeout = '5s';

-- Retire the legacy credential-injection rule tables. Nothing reads or writes
-- them: network credential rules now live in the effective network policy,
-- and credential reference retirement no longer checks the old attachment
-- table. Every release that still read these tables must be drained before
-- this migration runs.
--
-- The tables are dropped child-first: rules reference the ruleset version.
-- squawk-ignore ban-drop-table
DROP TABLE sandbox_network_credential_injection_rules;
-- squawk-ignore ban-drop-table
DROP TABLE sandbox_network_credential_injection_ruleset_version;
-- squawk-ignore ban-drop-table
DROP TABLE sandbox_credential_injection_rules;
