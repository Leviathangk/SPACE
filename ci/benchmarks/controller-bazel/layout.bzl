"""Expose a prepared OCI archive as an image input without registry publication."""

def _layout_impl(ctx):
    output = ctx.actions.declare_directory(ctx.label.name)
    ctx.actions.run_shell(
        inputs = [ctx.file.archive],
        outputs = [output],
        arguments = [output.path, ctx.file.archive.path],
        command = 'mkdir -p "$1" && tar -xf "$2" -C "$1"',
        mnemonic = "UnpackRuntimeImage",
    )
    return [DefaultInfo(files = depset([output]))]

oci_layout = rule(
    implementation = _layout_impl,
    attrs = {"archive": attr.label(allow_single_file = True, mandatory = True)},
)
