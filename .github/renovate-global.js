module.exports = {
  platform: "github",
  repositories: ["screwys/Rufin"],
  onboarding: false,
  requireConfig: "required",
  allowedCommands: [
    "^cargo run --locked -p xtask -- generate flatpak-sources$",
  ],
};
