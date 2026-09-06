namespace PrivateAIGateway.Windows;

internal static class RuntimePresentation
{
    internal static bool IsProtected(GatewayState state) =>
        state.Status == "verified" && !state.ConfigurationVerification;

    internal static bool ShowDevMode(GatewayState state) =>
        IsProtected(state) && !state.Config.RequireProductionOs;

    internal static string StatusLabel(GatewayState state) => state.Status switch
    {
        "verifying" when state.ConfigurationVerification => "Verifying configuration",
        "verifying" => "Starting",
        "verified" when state.ConfigurationVerification => "Configuration verified",
        "verified" when !state.Config.RequireProductionOs => "Protected · Dev mode",
        "verified" => "Protected",
        "blocked" => "Blocked",
        "error" => "Needs attention",
        _ => "Not protected",
    };

    internal static string SummaryLabel(GatewayState state) =>
        ShowDevMode(state) ? "Protected in dev mode" : StatusLabel(state);
}
