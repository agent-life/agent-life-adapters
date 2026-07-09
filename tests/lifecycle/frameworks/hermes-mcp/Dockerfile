# Hermes-as-MCP-host lifecycle image (WP-M4 release gate).
#
# Identical runtime to the plain hermes image — the ONLY difference is how alf is
# driven (the LLM agent calls `mcp_alf_*` tools instead of the harness running
# `alf` on the terminal), which is a config + stage concern, not an image one.
# So this layers on the already-built hermes image rather than repeating its
# heavy uv/node build.
#
# PREREQ: build the base first (a hermes run does this, or explicitly:
#   docker build -t alf-lifecycle-hermes tests/lifecycle/frameworks/hermes ).
# Then:  docker build -t alf-lifecycle-hermes-mcp tests/lifecycle/frameworks/hermes-mcp
#
# Invariants (D6): NO alf, NO secrets in any layer (the base already honours this;
# the alf binary is bind-mounted at run time, creds travel via --env-file / the
# per-server `env` block Hermes reads from config.yaml).
FROM alf-lifecycle-hermes

# Marker so `docker history` shows this is the MCP-host variant; no runtime change.
LABEL alf.lifecycle.variant="hermes-mcp"

CMD ["sleep", "infinity"]
