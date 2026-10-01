{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/master";
    make-shell.url = "github:nicknovitski/make-shell";

    fenix = {
      url = "github:nix-community/fenix/monthly";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-parts,
      systems,
      make-shell,
      fenix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ make-shell.flakeModules.default ];
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          fenixSys = fenix.packages.${system};
          openssl_pkgs = pkgs.openssl;
        in
        {
          make-shells.default = {
            env = {
              PKG_CONFIG_PATH = "${openssl_pkgs.dev}/lib/pkgconfig";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              LD_LIBRARY_PATH = pkgs.lib.strings.concatStrings (
                pkgs.lib.strings.intersperse ":" [
                  "$LD_LIBRARY_PATH"
                  "${openssl_pkgs.out}/lib"
                  "${pkgs.stdenv.cc.cc.lib}/lib"
                ]
              );
            };

            packages = [
              pkgs.pkg-config
              pkgs.openssl.dev
              pkgs.mold
              pkgs.clang
              pkgs.sccache
              pkgs.nodejs_22
              pkgs.sqlite
              pkgs.cmake

              (fenixSys.complete.withComponents [
                "cargo"
                "clippy"
                "rust-src"
                "rustc"
                "rustfmt"
              ])
              fenixSys.rust-analyzer
            ];
          };
        };
    };
}
