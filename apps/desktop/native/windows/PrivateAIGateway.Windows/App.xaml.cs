using Microsoft.UI.Xaml;
using System.Runtime.InteropServices;

namespace PrivateAIGateway.Windows;

public partial class App : Application
{
    public static MainWindow MainWindow { get; private set; } = null!;
    private static readonly Mutex Instance;
    private static readonly bool IsFirstInstance;

    static App()
    {
        Instance = new Mutex(true, "Local\\Dstack.PrivateAIGateway", out var firstInstance);
        IsFirstInstance = firstInstance;
    }

    public App() => InitializeComponent();

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (!IsFirstInstance)
        {
            NativeMethods.ActivateExistingWindow();
            Exit();
            return;
        }
        MainWindow = new MainWindow();
        MainWindow.Activate();
        if (Environment.GetCommandLineArgs().Contains("--autostart")) MainWindow.HideMainWindow();
    }

    private static class NativeMethods
    {
        [DllImport("user32.dll", EntryPoint = "FindWindowW", CharSet = CharSet.Unicode)]
        private static extern nint FindWindow(string? className, string windowName);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool ShowWindow(nint window, int command);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetForegroundWindow(nint window);

        internal static void ActivateExistingWindow()
        {
            var window = FindWindow(null, "Private AI Gateway");
            if (window == 0) return;
            ShowWindow(window, 9);
            SetForegroundWindow(window);
        }
    }
}
