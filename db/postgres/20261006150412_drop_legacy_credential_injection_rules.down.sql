SET LOCAL lock_timeout = '1s';
SET LOCAL statement_timeout = '5s';

-- Recreate the retired tables empty, with the baseline's constraints,
-- triggers, index, and grants. Dropped rows are not recovered.
CREATE TABLE public.sandbox_credential_injection_rules (
    sandbox_id uuid NOT NULL,
    version bigint NOT NULL,
    rules bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT sandbox_credential_injection_rules_pkey PRIMARY KEY (sandbox_id),
    CONSTRAINT sandbox_credential_injection_rules_sandbox_id_fkey FOREIGN KEY (sandbox_id) REFERENCES public.sandboxes(id) ON DELETE CASCADE
);

CREATE TABLE public.sandbox_network_credential_injection_ruleset_version (
    sandbox_id uuid NOT NULL,
    version bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT sandbox_network_credential_injection_ruleset_vers_version_check CHECK ((version > 0)),
    CONSTRAINT sandbox_network_credential_injection_ruleset_version_pkey PRIMARY KEY (sandbox_id),
    CONSTRAINT sandbox_network_credential_injection_ruleset_ve_sandbox_id_fkey FOREIGN KEY (sandbox_id) REFERENCES public.sandboxes(id) ON DELETE CASCADE
);

CREATE TABLE public.sandbox_network_credential_injection_rules (
    sandbox_id uuid NOT NULL,
    rule_id text NOT NULL,
    credential_reference_id uuid NOT NULL,
    transformation_rule jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT sandbox_network_credential_injection_rules_pkey PRIMARY KEY (sandbox_id, rule_id),
    CONSTRAINT sandbox_network_credential_injecti_credential_reference_id_fkey FOREIGN KEY (credential_reference_id) REFERENCES public.credential_references(id),
    CONSTRAINT sandbox_network_credential_injection_rules_sandbox_id_fkey FOREIGN KEY (sandbox_id) REFERENCES public.sandbox_network_credential_injection_ruleset_version(sandbox_id) ON DELETE CASCADE
);

CREATE INDEX sandbox_network_credential_injection_rules_reference_idx ON public.sandbox_network_credential_injection_rules USING btree (credential_reference_id);

CREATE TRIGGER network_credential_rules_set_updated_at BEFORE UPDATE ON public.sandbox_network_credential_injection_rules FOR EACH ROW EXECUTE FUNCTION public.set_updated_at();
CREATE TRIGGER network_credential_ruleset_set_updated_at BEFORE UPDATE ON public.sandbox_network_credential_injection_ruleset_version FOR EACH ROW EXECUTE FUNCTION public.set_updated_at();
CREATE TRIGGER sandbox_credential_injection_rules_set_updated_at BEFORE UPDATE ON public.sandbox_credential_injection_rules FOR EACH ROW EXECUTE FUNCTION public.set_updated_at();

GRANT SELECT ON TABLE public.sandbox_credential_injection_rules TO space_credmgr;
GRANT SELECT ON TABLE public.sandbox_network_credential_injection_rules TO space_credmgr;
GRANT SELECT ON TABLE public.sandbox_network_credential_injection_ruleset_version TO space_credmgr;
