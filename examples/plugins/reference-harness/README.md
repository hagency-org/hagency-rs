# Reference harness fixture

This deterministic `hagency.harness/v1` package demonstrates a host-managed
view and bounded prepare/release lifecycle. It never launches or prompts the
coding agent; the selected runtime plugin owns execution.

The required reference target and ownership values demonstrate digest-bound
template validation. They are echoed as prepared scope; this fixture does not
create or launch the target.

The package runs offline and requires no Herdr installation or provider login.
