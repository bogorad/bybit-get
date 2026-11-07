{
  self,
  home-manager,
  pkgs,
}:

let
  mkHome =
    extra:
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        self.homeManagerModules.default
        {
          home = {
            username = "bybit-test";
            homeDirectory = "/home/bybit-test";
            stateVersion = "26.05";
          };
          programs.bybit-get = {
            enable = true;
            package = pkgs.hello;
          };
        }
        extra
      ];
    }).config;
  defaults = mkHome { };
  custom = mkHome {
    programs.bybit-get = {
      apiKeyFile = "/runtime/test-api-key";
      apiSecretFile = "/runtime/test-api-secret";
      baseURL = "https://api.bybit.eu";
    };
  };
in
assert builtins.elem pkgs.hello defaults.home.packages;
assert builtins.elem pkgs.hello custom.home.packages;
pkgs.runCommand "bybit-get-home-manager-check"
  {
    nativeBuildInputs = [
      pkgs.remarshal
      pkgs.jq
    ];
    defaultConfig = defaults.xdg.configFile."bybit-get/config.toml".source;
    customConfig = custom.xdg.configFile."bybit-get/config.toml".source;
  }
  ''
    toml2json "$defaultConfig" > default.json
    jq -e '
      (keys == ["api_key_file", "api_secret_file", "base_url"]) and
      (.api_key_file == "/run/secrets/api_keys/bybit/api_key") and
      (.api_secret_file == "/run/secrets/api_keys/bybit/api_secret") and
      (.base_url == "https://api.bybit.com")
    ' default.json
    toml2json "$customConfig" > custom.json
    jq -e '
      (keys == ["api_key_file", "api_secret_file", "base_url"]) and
      (.api_key_file == "/runtime/test-api-key") and
      (.api_secret_file == "/runtime/test-api-secret") and
      (.base_url == "https://api.bybit.eu")
    ' custom.json
    mkdir -p "$out"
    cp default.json custom.json "$out/"
  ''
