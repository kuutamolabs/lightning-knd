{
  description = "Bitcoin Lightning node";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable-small";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

  };

  nixConfig.extra-substituters = [
    "https://cache.garnix.io"
  ];
  nixConfig.extra-trusted-public-keys = [
    "cache.garnix.io:CTFPyKSLcx5RMJKfLo5EEPUObbA78b0YQ2DTCJXqr9g="
  ];

  outputs = { self, flake-parts, nixpkgs, ... }:
    flake-parts.lib.mkFlake { inherit self; } {
      imports = [
        ./nix/pkgs/flake-module.nix
        ./nix/modules/flake-module.nix
        ./nix/checks/flake-module.nix
        ./nix/shell.nix
      ];
      systems = [ "x86_64-linux" ];

      perSystem = { inputs', system, ... }: {
        _module.args.pkgs = import nixpkgs {
          inherit system;
          config.allowUnfreePredicate = (pkg: builtins.elem
            (builtins.parseDrvName pkg.pname).name [
            "cockroach"
          ]);
        };
      };
    };
}
