"""Individual smoke targets using an invocation-owned pool of isolated stacks."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")
load(":targets.bzl", "KIND_SCENARIOS")

def kind_functional_tests():
    targets = []
    for surface, scenario in KIND_SCENARIOS:
        name = surface + "__" + scenario.replace(".", "__")
        sh_test(
            name = name,
            env_inherit = ["PATH"],
            srcs = ["scenario.sh"],
            args = [surface, scenario],
            data = ["pool.py"],
            size = "large",
            # The first lease includes the cold fixture bootstrap (up to 40m).
            timeout = "eternal",
            # Privileged real-substrate tests must execute on a provisioned
            # worker and produce fresh evidence against its current kernel.
            tags = ["manual", "external", "no-sandbox", "no-remote"],
        )
        targets.append(":" + name)
    native.test_suite(
        name = "functional",
        tests = targets,
        tags = ["manual"],
    )
