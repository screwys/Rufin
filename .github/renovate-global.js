module.exports = {
  platform: "github",
  repositories: ["rufin-app/rufin"],
  onboarding: false,
  requireConfig: "required",
  allowedCommands: [
    "^cargo run --locked -p xtask -- generate flatpak-sources$",
  ],
};
