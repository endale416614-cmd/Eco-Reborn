using System;
using EcoRebornLauncher;

internal static class LauncherChecks
{
    private static int Main()
    {
        Assert(LauncherForm.QuoteArgument(@"C:\ECO Client\Start-EcoReborn.ps1") ==
            "\"C:\\ECO Client\\Start-EcoReborn.ps1\"", "paths with spaces stay one argument");
        Assert(LauncherForm.QuoteArgument("C:\\folder\\name\"quoted\"\\") ==
            "\"C:\\folder\\name\\\"quoted\\\"\\\\\"", "quotes and trailing slashes are escaped");
        Assert(LauncherForm.IsStartedMarker("ECO_LAUNCHER_CLIENT_STARTED"), "exact startup marker recognized");
        Assert(!LauncherForm.IsStartedMarker("eco_launcher_client_started") &&
            !LauncherForm.IsStartedMarker("ECO_LAUNCHER_CLIENT_STARTED extra"), "marker matching is exact");
        string failure = LauncherForm.FailureMessage(@"F:\ECO Path");
        Assert(failure.Contains(@"F:\ECO Path\offline\reports") && failure.Contains("ports are free"),
            "failure guidance stays generic and actionable");
        string isolation = LauncherForm.IsolationMessage(@"F:\ECO Path");
        Assert(isolation.Contains("Repair Isolation.cmd") && isolation.Contains(@"F:\ECO Path"),
            "missing isolation names the repair file and folder");
        Assert(LauncherForm.IsolationMissingExitCode == 3, "isolation exit code matches Start-EcoReborn.ps1");
        return 0;
    }

    private static void Assert(bool condition, string message)
    {
        if (!condition) throw new Exception("Launcher check failed: " + message);
    }
}
