gui:
{
  config,
  lib,
  osConfig ? { },
  ...
}:
let
  cfg = config.programs.flydigictl;

  # Stylix is not a dependency and is not required; it is simply the thing most
  # likely to already know what colours this machine uses. Where it is present,
  # its scheme is the default rather than something to be wired up by hand.
  scheme = if config.lib ? stylix then config.lib.stylix.colors.withHashtag else null;

  fromScheme =
    if scheme == null then
      { }
    else
      {
        background = scheme.base00;
        text = scheme.base05;
        primary = scheme.base0D;
        success = scheme.base0B;
        warning = scheme.base0A;
        danger = scheme.base08;
      };
in
{
  options.programs.flydigictl = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = osConfig.services.flydigictl.enable or false;
      example = true;
      description = ''
        Whether to install the Flydigi cooler interface.

        On by default where this machine runs the daemon: the interface is what
        that daemon is driven by, and it is of no use anywhere else.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = gui;
      description = "The flydigictl-gui package to use.";
    };

    palette = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = fromScheme;
      defaultText = lib.literalMD "the Stylix scheme, where Stylix is in use";
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
        for light or dark.

        Roles are `background`, `text`, `primary`, `success`, `warning` and
        `danger`; anything else is ignored. Partial sets are not: the interface
        wants all six, or it falls back to the light and dark it knows. Set this
        to `{ }` to keep it that way.
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
