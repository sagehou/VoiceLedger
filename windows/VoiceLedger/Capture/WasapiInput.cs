using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using NAudio.CoreAudioApi;
using NAudio.CoreAudioApi.Interfaces;
using NAudio.Wave;

namespace VoiceLedger.Capture;

internal static class EndpointCatalog
{
    internal static IReadOnlyList<CaptureEndpoint> Read()
    {
        using var devices = new MMDeviceEnumerator();
        var result = new List<CaptureEndpoint>();
        ReadDirection(DataFlow.Capture, CaptureSource.Mic);
        ReadDirection(DataFlow.Render, CaptureSource.System);
        return result;

        void ReadDirection(DataFlow flow, CaptureSource source)
        {
            string? defaultId = null;
            if (devices.HasDefaultAudioEndpoint(flow, Role.Multimedia))
            {
                using MMDevice defaultDevice = devices.GetDefaultAudioEndpoint(flow, Role.Multimedia);
                defaultId = defaultDevice.ID;
            }
            foreach (MMDevice device in devices.EnumerateAudioEndPoints(flow, DeviceState.All))
            {
                using (device)
                {
                    string format = "Format unavailable";
                    DeviceState state = device.State;
                    if (state == DeviceState.Active)
                    {
                        try
                        {
                            using AudioClient client = device.AudioClient;
                            format = client.MixFormat.ToString();
                        }
                        catch (COMException) { }
                    }
                    result.Add(new CaptureEndpoint(device.ID, device.FriendlyName, source, state,
                        StringComparer.Ordinal.Equals(defaultId, device.ID), format));
                }
            }
        }
    }
}

internal sealed class WasapiInput : ICaptureInput
{
    private readonly MMDeviceEnumerator enumerator = new();
    private readonly AutoResetEvent packetReady = new(false);
    private readonly EndpointWatch watch;
    private MMDevice? device;
    private AudioClient? client;
    private AudioCaptureClient? capture;
    private bool registered;

    private WasapiInput(CaptureEndpoint endpoint, Action<CaptureEnd> interrupted)
    {
        watch = new EndpointWatch(endpoint.Id, interrupted);
    }

    public WaveFormat Format { get; private set; } = null!;
    public int BufferFrames { get; private set; }
    public WaitHandle PacketReady => packetReady;

    internal static ICaptureInput Open(CaptureEndpoint endpoint, Action<CaptureEnd> interrupted)
    {
        var input = new WasapiInput(endpoint, interrupted);
        try
        {
            input.Initialize(endpoint);
            return input;
        }
        catch
        {
            input.Dispose();
            throw;
        }
    }

    private void Initialize(CaptureEndpoint endpoint)
    {
        Marshal.ThrowExceptionForHR(enumerator.RegisterEndpointNotificationCallback(watch));
        registered = true;
        device = enumerator.GetDevice(endpoint.Id);
        DataFlow requested = endpoint.Source == CaptureSource.Mic ? DataFlow.Capture : DataFlow.Render;
        if (device.State != DeviceState.Active || device.DataFlow != requested)
        {
            throw new IOException("The selected endpoint is unavailable.");
        }

        client = device.AudioClient;
        Format = client.MixFormat;
        var flags = AudioClientStreamFlags.EventCallback | AudioClientStreamFlags.NoPersist;
        if (endpoint.Source == CaptureSource.System)
        {
            flags |= AudioClientStreamFlags.Loopback;
        }
        client.Initialize(AudioClientShareMode.Shared, flags, 0, 0, Format, Guid.Empty);
        client.SetEventHandle(packetReady.SafeWaitHandle.DangerousGetHandle());
        BufferFrames = client.BufferSize;
        capture = client.AudioCaptureClient;
    }

    public void Start() => client!.Start();
    public void Stop() => client!.Stop();

    public bool TryRead(PacketConsumer consume)
    {
        AudioCaptureClient reader = capture!;
        if (reader.GetNextPacketSize() == 0)
        {
            return false;
        }
        nint pointer = reader.GetBuffer(out int frames, out AudioClientBufferFlags flags, out long position, out long qpc);
        try
        {
            if (frames > 0)
            {
                consume(pointer, new PacketStamp(frames, position, qpc, flags));
            }
        }
        finally
        {
            reader.ReleaseBuffer(frames);
        }
        return frames > 0;
    }

    public void Dispose()
    {
        try
        {
            if (registered)
            {
                enumerator.UnregisterEndpointNotificationCallback(watch);
                registered = false;
            }
        }
        finally
        {
            client?.Dispose();
            device?.Dispose();
            packetReady.Dispose();
            enumerator.Dispose();
        }
    }
}

internal sealed class EndpointWatch(string selectedId, Action<CaptureEnd> interrupted) : IMMNotificationClient
{
    public void OnDeviceStateChanged(string deviceId, DeviceState newState)
    {
        if (newState != DeviceState.Active)
        {
            OnDeviceRemoved(deviceId);
        }
    }

    public void OnDeviceRemoved(string deviceId)
    {
        if (StringComparer.Ordinal.Equals(deviceId, selectedId))
        {
            interrupted(CaptureEnd.EndpointLost);
        }
    }

    public void OnDeviceAdded(string deviceId) { }
    public void OnDefaultDeviceChanged(DataFlow flow, Role role, string defaultDeviceId) { }

    public void OnPropertyValueChanged(string deviceId, PropertyKey key)
    {
        if (StringComparer.Ordinal.Equals(deviceId, selectedId)
            && (Matches(key, PropertyKeys.PKEY_AudioEngine_DeviceFormat) || Matches(key, PropertyKeys.PKEY_AudioEngine_OEMFormat)))
        {
            interrupted(CaptureEnd.FormatChanged);
        }
    }

    private static bool Matches(PropertyKey left, PropertyKey right) =>
        left.formatId == right.formatId && left.propertyId == right.propertyId;
}
