# Input and output schema constructors shared by every adapter module.
#
# Not a NixOS module -- `import`ed directly, and deliberately absent from
# ./default.nix. It exists because every adapter module carried its own copy of
# the same helper:
#
#     object = properties: required: {
#       type = "object";
#       additionalProperties = true;   # <- accepts anything, including typos
#       inherit properties required;
#     };
#
# Nine copies, and an operation whose declared properties were incomplete still
# validated, because any field at all was allowed. The validation in
# workenv-core/src/adapter.rs had nothing to reject, so a misspelled input key
# was silently ignored rather than refused, and the adapter ran with a default.
# One definition cannot drift per module the way nine can.
{
  # Exactly the declared properties, and nothing else. The property set has to be
  # what the adapter actually reads out of `request.input` -- written from the
  # documentation instead of the code, a closed schema rejects input the adapter
  # honours, which is a worse failure than the one being fixed.
  closed = properties: required: {
    type = "object";
    additionalProperties = false;
    inherit properties required;
  };

  # An operation that takes no input at all, and says so.
  none = {
    type = "object";
    additionalProperties = false;
    properties = { };
    required = [ ];
  };

  # Adapter output stays open. It is the adapter's own report rather than a
  # caller-supplied argument, adapters add fields to it between releases, and a
  # closed output schema would turn a richer report into a hard failure. Required
  # fields are how output is constrained; see modules/orchard.nix.
  output = {
    type = "object";
    additionalProperties = true;
  };
}
