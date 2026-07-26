gui:
{
  config,
  lib,
  ...
}:
let
  cfg = config.programs.flydigictl;
in
{
  options.programs.flydigictl = {
    enable = lib.mkEnableOption "the Flydigi cooler interface";

    package = lib.mkOption {
      type = lib.types.package;
      default = gui;
      description = "The flydigictl-gui package to use.";
    };

    palette = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = lib.literalExpression ''
        {
          background = "#1f2430";
          text = "#cccac2";
          primary = "#73d0ff";
          success = "#d5ff80";
          warning = "#ffd173";
          danger = "#f28779";
        }
      '';
      description = ''
        Colours for the interface, as `rrggbb`.

        The interface draws itself rather than through GTK or Qt, so no desktop
        setting reaches it: left alone it can only tell whether the system asked
        for light or dark. Fill this in - from a colour scheme generator, or by
        hand - and it uses these instead.

        Roles are `background`, `text`, `primary`, `success`, `warning` and
        `danger`; anything else is ignored. Partial sets are not: the interface
        wants all six, or it falls back to the light and dark it knows.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."flydigictl/palette.json" = lib.mkIf (cfg.palette != { }) {
      text = builtins.toJSON cfg.palette;
    };
  };
}
