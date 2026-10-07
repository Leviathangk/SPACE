SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- Reverting drops tenant template-build admission and retention state. An API
-- release that still reads these tables cannot admit template builds, so roll
-- that release back first.
DROP TABLE IF EXISTS template_build_input_retention;
DROP TABLE IF EXISTS template_build_input_charges;
DROP TABLE IF EXISTS template_build_admissions;
