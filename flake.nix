{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    naersk = {
      url = "github:nix-community/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    utils = {
      url = "github:numtide/flake-utils";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, utils, naersk }:
    utils.lib.eachDefaultSystem (system:
      let
        buildInputs = with pkgs; [ openssl pkg-config ];
        naersk-lib = pkgs.callPackage naersk { };
        pkgs = import nixpkgs { inherit system; };
      in {
        defaultPackage = naersk-lib.buildPackage {
          inherit buildInputs;
          src = ./.;
        };

        defaultApp = utils.lib.mkApp { drv = self.defaultPackage."${system}"; };

        devShell = with pkgs;
          mkShell {
            buildInputs = buildInputs ++ [
              cargo
              pre-commit
              rust-analyzer
              rustPackages.clippy
              rustc
              rustfmt
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      });
}
