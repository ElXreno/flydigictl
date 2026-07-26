gui:
{
  config,
  lib,
  ...
}:
let
  cfg = config.programs.flydigictl;

  scheme = if config.lib ? stylix then config.lib.stylix.colors.withHashtag else null;

  # The whole scheme rather than the six roles: given base01 through base03 the
  # interface shades panels and lines the way the rest of the desktop does,
  # instead of inventing them from the background.
  fromScheme =
    if scheme == null then
      { }
    else
      lib.genAttrs (map (index: "base0${index}") [
        "0"
        "1"
        "2"
        "3"
        "4"
        "5"
        "6"
        "7"
        "8"
        "9"
        "A"
        "B"
        "C"
        "D"
        "E"
        "F"
      ]) (name: scheme.${name});
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

        Either a base16 scheme, `base00` through `base0F`, or the six roles it
        uses directly: `background`, `text`, `primary`, `success`, `warning`,
        `danger`. A scheme is worth more, since `base01` to `base03` also give
        it the shades for panels and lines. Partial sets are ignored, and `{ }`
        leaves it following the light and dark preference.
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
