#!/bin/sh
# Idiomatic two-agent Hermes setup. Runs INSIDE the container.
#
# Hermes' native multi-agent surface is PROFILES: each profile is a fully
# isolated HERMES_HOME. The default profile is ~/.hermes itself; named profiles
# live under ~/.hermes/profiles/<name>/ and each gets a ~/.local/bin/<name>
# command alias. So "two agents" = two named profiles.
set -eu

hermes profile create agent_a
hermes profile create agent_b
hermes profile list
