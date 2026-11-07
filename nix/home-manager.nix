self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.bybit-get;
  toml = pkgs.formats.toml { };
in
{
  options.programs.bybit-get = {
    enable = lib.mkEnableOption "the Bybit Card transaction history CLI";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "inputs.bybit-get.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The bybit-get package to install.";
    };
    apiKeyFile = lib.mkOption {
      type = lib.types.str;
      default = "/run/secrets/api_keys/bybit/api_key";
      example = "/run/user/1000/secrets/bybit-api-key";
      description = "Runtime file that contains the API key. This is a string so Nix does not copy its contents into the store.";
    };
    apiSecretFile = lib.mkOption {
      type = lib.types.str;
      default = "/run/secrets/api_keys/bybit/api_secret";
      example = "/run/user/1000/secrets/bybit-api-secret";
      description = "Runtime file that contains the API secret. This is a string so Nix does not copy its contents into the store.";
    };
    baseURL = lib.mkOption {
      type = lib.types.str;
      default = "https://api.bybit.com";
      description = "Official HTTPS Bybit API origin.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    xdg.configFile."bybit-get/config.toml".source = toml.generate "bybit-get-config.toml" {
      api_key_file = cfg.apiKeyFile;
      api_secret_file = cfg.apiSecretFile;
      base_url = cfg.baseURL;
    };
  };
}
