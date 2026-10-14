using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace EcoRebornLauncher
{
    internal static class Program
    {
        private const string MutexName = "Local\\EcoReborn.Launcher";

        [STAThread]
        private static void Main()
        {
            using (Mutex mutex = new Mutex(false, MutexName))
            {
                bool acquired;
                try { acquired = mutex.WaitOne(0); }
                catch (AbandonedMutexException) { acquired = true; }
                if (!acquired)
                {
                    MessageBox.Show("Eco Reborn is already running.", "Eco Reborn",
                        MessageBoxButtons.OK, MessageBoxIcon.Information);
                    return;
                }

                try
                {
                    Application.EnableVisualStyles();
                    Application.SetCompatibleTextRenderingDefault(false);
                    Application.Run(new LauncherForm());
                }
                finally { mutex.ReleaseMutex(); }
            }
        }
    }

    internal sealed class LauncherForm : Form
    {
        private const string StartedMarker = "ECO_LAUNCHER_CLIENT_STARTED";
        // Start-EcoReborn.ps1 exits with this when the firewall isolation rules are missing.
        internal const int IsolationMissingExitCode = 3;
        private readonly string _root;
        private readonly Label _status;
        private readonly ProgressBar _progress;
        private Process _process;
        private bool _finished;

        internal LauncherForm()
        {
            _root = Path.GetDirectoryName(Application.ExecutablePath);
            Text = "Eco Reborn";
            FormBorderStyle = FormBorderStyle.FixedDialog;
            StartPosition = FormStartPosition.CenterScreen;
            ClientSize = new Size(420, 170);
            Icon = System.Drawing.Icon.ExtractAssociatedIcon(Application.ExecutablePath);
            MaximizeBox = false;
            MinimizeBox = false;
            ShowInTaskbar = true;
            BackColor = Color.FromArgb(24, 27, 34);
            ForeColor = Color.WhiteSmoke;

            Label title = new Label();
            title.Text = "ECO";
            title.Font = new Font("Segoe UI", 27, FontStyle.Bold);
            title.ForeColor = Color.FromArgb(235, 190, 100);
            title.AutoSize = true;
            title.Location = new Point(28, 20);
            Controls.Add(title);

            Label subtitle = new Label();
            subtitle.Text = "Offline";
            subtitle.Font = new Font("Segoe UI", 12, FontStyle.Regular);
            subtitle.AutoSize = true;
            subtitle.Location = new Point(33, 73);
            Controls.Add(subtitle);

            _status = new Label();
            _status.Text = "Starting local game...";
            _status.Font = new Font("Segoe UI", 9, FontStyle.Regular);
            _status.AutoSize = true;
            _status.Location = new Point(33, 112);
            Controls.Add(_status);

            _progress = new ProgressBar();
            _progress.Style = ProgressBarStyle.Marquee;
            _progress.MarqueeAnimationSpeed = 26;
            _progress.Location = new Point(33, 138);
            _progress.Size = new Size(354, 8);
            Controls.Add(_progress);

            Shown += StartSession;
            FormClosing += HandleClosing;
        }

        private async void StartSession(object sender, EventArgs e)
        {
            string script = Path.Combine(_root, "offline", "Start-EcoReborn.ps1");
            string client = Path.Combine(_root, "eco_jp", "eco.exe");
            string server = Path.Combine(_root, "offline", "target", "release", "eco-reborn.exe");
            string powershell = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System),
                "WindowsPowerShell", "v1.0", "powershell.exe");

            string missing = !File.Exists(script) ? "The offline startup script is missing." :
                !File.Exists(client) ? "The ECO client executable is missing." :
                !File.Exists(server) ? "The offline server executable is missing. Build offline first." :
                !File.Exists(powershell) ? "Windows PowerShell 5.1 could not be found." : null;
            if (missing != null)
            {
                FinishWithError(missing);
                return;
            }

            bool started = false;
            try
            {
                ProcessStartInfo start = new ProcessStartInfo();
                start.FileName = powershell;
                start.Arguments = "-NoProfile -NonInteractive -File " + QuoteArgument(script);
                start.WorkingDirectory = _root;
                start.UseShellExecute = false;
                start.CreateNoWindow = true;
                start.WindowStyle = ProcessWindowStyle.Hidden;
                start.RedirectStandardOutput = true;
                start.RedirectStandardError = true;

                _process = new Process();
                _process.StartInfo = start;
                _process.OutputDataReceived += ReadOutput;
                _process.ErrorDataReceived += DrainError;
                if (!_process.Start())
                {
                    _process.Dispose();
                    _process = null;
                    FinishWithError("Windows PowerShell could not be started.");
                    return;
                }
                started = true;
            }
            catch
            {
                if (started) return;
                DisposeProcess();
                FinishWithError("Windows PowerShell could not start the offline session.");
                return;
            }

            // Once started, keep supervising even if stream setup unexpectedly fails.
            try { _process.BeginOutputReadLine(); } catch { }
            try { _process.BeginErrorReadLine(); } catch { }

            int exitCode;
            try
            {
                exitCode = await Task.Run(delegate
                {
                    _process.WaitForExit();
                    return _process.ExitCode;
                });
            }
            catch { exitCode = 1; }
            Complete(exitCode);
        }

        private void ReadOutput(object sender, DataReceivedEventArgs e)
        {
            if (IsStartedMarker(e.Data))
                OnUi(HideAfterClientStart);
        }

        private void DrainError(object sender, DataReceivedEventArgs e)
        {
            // Read every stderr event without retaining or showing private process output.
        }

        private void HideAfterClientStart()
        {
            if (!_finished && !IsDisposed) Hide();
        }

        private void Complete(int exitCode)
        {
            if (_finished) return;
            _finished = true;
            DisposeProcess();
            if (exitCode == 0)
            {
                Close();
                return;
            }
            FinishWithError(exitCode == IsolationMissingExitCode ? IsolationMessage(_root) : FailureMessage(_root));
        }

        private void FinishWithError(string message)
        {
            _finished = true;
            DisposeProcess();
            _status.Text = "Eco Reborn could not finish this session.";
            _progress.Style = ProgressBarStyle.Blocks;
            Show();
            Activate();
            MessageBox.Show(this, message, "Eco Reborn", MessageBoxButtons.OK, MessageBoxIcon.Error);
            Close();
        }

        private void HandleClosing(object sender, FormClosingEventArgs e)
        {
            if (!_finished && _process != null)
            {
                e.Cancel = true;
                Hide();
            }
        }

        private void OnUi(MethodInvoker action)
        {
            if (IsDisposed || !IsHandleCreated) return;
            try { BeginInvoke(action); }
            catch (InvalidOperationException) { }
        }

        private void DisposeProcess()
        {
            if (_process == null) return;
            _process.OutputDataReceived -= ReadOutput;
            _process.ErrorDataReceived -= DrainError;
            _process.Dispose();
            _process = null;
        }

        internal static string QuoteArgument(string value)
        {
            StringBuilder quoted = new StringBuilder("\"");
            int slashes = 0;
            foreach (char ch in value)
            {
                if (ch == '\\') { slashes++; continue; }
                if (ch == '"')
                {
                    quoted.Append('\\', slashes * 2 + 1).Append('"');
                    slashes = 0;
                    continue;
                }
                quoted.Append('\\', slashes).Append(ch);
                slashes = 0;
            }
            quoted.Append('\\', slashes * 2).Append('"');
            return quoted.ToString();
        }

        internal static bool IsStartedMarker(string line)
        {
            return String.Equals(line, StartedMarker, StringComparison.Ordinal);
        }

        internal static string FailureMessage(string root)
        {
            return "The session ended with an error. Check that the server is built and " +
                "local ports are free. Check the privacy-safe reports in " +
                Path.Combine(root, "offline", "reports") + ".";
        }

        internal static string IsolationMessage(string root)
        {
            return "Eco Reborn did not start because its network isolation is not set up for this folder.\n\n" +
                "Double-click \"Repair Isolation.cmd\" in " + root + " and approve the Windows prompt, " +
                "then start Eco Reborn again. This is needed once after the folder is moved or renamed.";
        }
    }
}
