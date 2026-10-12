-- Unknown historical materializations stay unknown; new writers allocate identities.
ALTER TABLE sandbox_k8s_executions ADD COLUMN materialization_id UUID;
