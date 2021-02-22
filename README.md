# rfbproxy

An RFB proxy that enables WebSockets and audio.

This crate proxies a TCP Remote Framebuffer server connection and exposes a
WebSocket endpoint, translating the connection between them. It can optionally
enable audio using the [Replit Audio RFB extension](#replit-audio-rfb-extension) if the
`--enable-audio` flag is passed or the `VNC_ENABLE_EXPERIMENTAL_AUDIO`
environment variable is set to a non-empty value.

# Replit Audio RFB extension

This uses a proposed extension to the [RFB
protocol](https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst) in
order to negotiate and transmit encoded audio. This is the main difference from
the pre-existing [QEMU Audio
messages](https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst#qemu-audio-client-message).

## Encodings

This registers the following pseudo-encodings:

| Number     | Name                          |
|----------- | ------------------------------|
| 0x52706C41 | Replit Audio Pseudo-encoding |

A client that supports this encoding is indicating that it is able to receive
an encoded audio data stream. If a server wishes to send encoded audio data, it
will send a pseudo-rectangle with the following contents:

| No. of bytes           | Type        | Description        |
|------------------------|-------------|--------------------|
| 2                      | `U16`       | _version_          |
| 2                      | `U16`       | _number-of-codecs_ |
| 2 * _number-of-codecs_ | `U16` array | _codecs_           |

The supported codecs are as follow:

| Codec | Description                 |
|-------|-----------------------------|
| 0     | Opus codec, WebM container  |
| 1     | MP3 codec, MPEG-1 container |

After receiving this notification, clients may optionally use the [Replit Audio
Client Message](#replit-audio-client-client-to-server-messages).

## Client to Server Messages

This registers the following message types:

| Number | Name                        |
|--------|-----------------------------|
| 245    | Replit Audio Client Message |

This message may only be sent if the client has previously received a
_FrameBufferUpdate_ that confirms support for the intended message-type. Every
`Replit Audio Client Message` begins with a standard header

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  |         | _submessage-type_ |
| 2            | `U16` |         | _payload-length_  |

This header is then followed by arbitrary data of length _payload-length_, and
whose format is determined by the _submessage-type_. Possible values for
_submessage-type_ and their associated minimum versions are

| Submessage Type | Minimum version | Description                                                                       |
|-----------------|-----------------|-----------------------------------------------------------------------------------|
| 0               | 0               | [Start Encoder](#replit-audio-client-start-encoder-message)                       |
| 1               | 0               | [Frame Request](#replit-audio-client-frame-request-message)                       |
| 2               | 0               | [Start Continuous Updates](#replit-audio-client-start-continuous-updates-message) |

### Replit Audio Client Start Encoder Message

This submessage allows the client to request the server to start audio capture
with the provided configuration

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  | 0       | _submessage-type_ |
| 2            | `U16` | 6       | _payload-length_  |
| 1            | `U8`  |         | _enabled_         |
| 1            | `U8`  |         | _channels_        |
| 2            | `U16` |         | _codec_           |
| 2            | `U16` |         | _kbytes_per_sec_  |

After invoking this operation, the client will receive a [Replit Audio Server
Start Encoder Message](#replit-audio-server-start-encoder-message) with the
result of the operation.

Valid values for the _enabled_ field are 0, which disables/stops the audio
encoder, and 1, which starts the audio encoder. Valid values for the _channels_
field are 1 (Mono audio) and 2 (Stereo audio). Valid values for the _codec_
field are the ones sent by the server in the [Replit Audio
Pseudo-encoding](#encodings) pseudo-rect. Valid values for the _kbytes_per_sec_
field are codec-dependent. The Opus codec achieves good performance with 32,
whereas the MP3 codec might require 128 for a comparable experience.

### Replit Audio Client Frame Request Message

This submessage allows the client to request the server for a single audio
frame. The length of an audio frame is codec-dependent, but is typically
between 5 and 40 milliseconds. Each frame is encoded with the parameters chosen
by the [Start Encoder](#replit-audio-client-start-encoder-message) message.
The client MUST send a [Start
Encoder](#replit-audio-client-start-encoder-message) message and have received
acknowledgement from the server that the chosen parameters are valid prior to
sending this message.

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  | 1       | _submessage-type_ |
| 2            | `U16` | 0       | _payload-length_  |

After invoking this operation, the client will receive a [Replit Audio Server
Frame Response Message](#replit-audio-server-frame-response-message) with the
encoded audio frame in the corresponding container format.

### Replit Audio Client Start Continuous Updates Message

This submessage allows the client to request the server send audio frames
continuously, which saves bandwidth and reduces audio latency incurred by the
TCP stack by half compared to requesting frames individually. The length of an
audio frame is codec-dependent, but is typically between 5 and 40 milliseconds.
Each frame is encoded with the parameters chosen by the [Start
Encoder](#replit-audio-client-start-encoder-message) message.  The client MUST
send a [Start Encoder](#replit-audio-client-start-encoder-message) message and
have received acknowledgement from the server that the chosen parameters are
valid prior to sending this message.

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  | 1       | _submessage-type_ |
| 2            | `U16` | 0       | _payload-length_  |

After invoking this operation, the client will receive a [Replit Audio Server
Start Continuous Updates
Message](#replit-audio-server-start-continuous-updates-message) with the result
of the operation. If the operation was successful, that message will be
followed by [Replit Audio Server Frame Response
Message](#replit-audio-server-frame-response-message) messages with and encoded
audio frame in the corresponding container format.

Once audio frames start being continuously sent, this can be stopped by sending
a [Start Encoder](#replit-audio-client-start-encoder-message) message with the
_enabled_ field set to `0`. Due to inherent race conditions in the protocol,
after disabling the encoder, the client may still receive further [Replit Audio
Server Frame Response Message](#replit-audio-server-frame-response-message)
messages, but once the server acknowledges the receipt of the [Start
Encoder](#replit-audio-client-start-encoder-message) message, no further audio
frames will be sent.

## Server to Client Messages

This registers the following message types:

| Number | Name                        |
|--------|-----------------------------|
| 245    | Replit Audio Server Message |

This message may only be sent if the client has previously sent a [Replit Audio
Client Message](#replit-audio-client-message) that confirms support for the
intended message-type. Every `Replit Audio Server Message` begins with a
standard header

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  |         | _submessage-type_ |
| 2            | `U16` |         | _payload-length_  |

This header is then followed by arbitrary data of length _payload-length_, and
whose format is determined by the _submessage-type_. Possible values for
_submessage-type_ and their associated minimum versions are

| Submessage Type | Minimum version | Description                                                                       |
|-----------------|-----------------|-----------------------------------------------------------------------------------|
| 0               | 0               | [Start Encoder](#replit-audio-server-start-encoder-message)                       |
| 1               | 0               | [Frame Request](#replit-audio-server-frame-request-message)                       |
| 2               | 0               | [Start Continuous Updates](#replit-audio-server-start-continuous-updates-message) |

### Replit Audio Server Start Encoder Message

This submessage is a response to the [Replit Audio Client Start Encoder
Message](#replit-audio-client-start-encoder-message), and acknowledges the
receipt and/or support for the requested configuration

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  | 0       | _submessage-type_ |
| 2            | `U16` | 1       | _payload-length_  |
| 1            | `U8`  |         | _enabled_         |

If the parameters in the [Replit Audio Client Start Encoder
Message](#replit-audio-client-start-encoder-message) were valid and the server
was able to successfully start an audio capture session, the value of _enabled_
will be 1. Otherwise it will be 0.

After receiveing this message with _enabled_ set to 1, the client can send
other [Replit Audio Client Message](#client-to-server-messages) messages.

### Replit Audio Server Frame Request Message

This submessage contains audio data for a single audio frame wrapped in the
container format associated with it. The length of an audio frame is
codec-dependent, but is typically between 5 and 40 milliseconds. The frame is
encoded with the parameters chosen by the [Start
Encoder](#replit-audio-client-start-encoder-message) message. This is a
response to either the [Replit Audio Client Frame Request
Message](#replit-audio-client-frame-request-message) or the [Replit Audio
Client Start Continuous Updates
Message](#replit-audio-client-start-continuous-updates-message).

| No. of bytes   | Type       | [Value]           | Description       |
|----------------|------------|-------------------|-------------------|
| 1              | `U8`       | 245               | _message-type_    |
| 1              | `U8`       | 1                 | _submessage-type_ |
| 2              | `U16`      | 4 + _data-length_ | _payload-length_  |
| 4              | `U32`      |                   | _timestamp_       |
| _data-length_  | `U8` array |                   | _data_            |

The most significant bit of _timestamp_ denotes whether the audio frame
contains a start-of-stream header or is otherwise a keyframe, which enables
clients to use this information for seeking purposes. Servers SHOULD send
keyframes every few seconds / minutes to allow clients to re-synchronize with
the stream. The 31 least significant bits of _timestamp_ contain the number of
milliseconds from the first audio frame that was captured in the session since
the [Start Encoder](#replit-audio-client-start-encoder-message) message was
acknowledged by the server. _data_ SHOULD be a self-contained audio frame, and
all the audio frames should be concatenable into a valid audio stream.
Furthermore, dropping of a non-keyframe SHOULD not cause the client to
de-synchronize, and SHOULD be recoverable by inserting silence for the duration
of the dropped frame.

### Replit Audio Server Start Continuous Updates Message

This submessage is a response to the [Replit Audio Client Start Continuous
Updates Message](#replit-audio-client-start-continuous-updates-message), and
acknowledges the receipt of it and signals the client that the server will send
[Replit Audio Server Frame Request
Message](#replit-audio-server-frame-request-message) messages continuously.

| No. of bytes | Type  | [Value] | Description       |
|--------------|-------|---------|-------------------|
| 1            | `U8`  | 245     | _message-type_    |
| 1            | `U8`  | 0       | _submessage-type_ |
| 2            | `U16` | 1       | _payload-length_  |
| 1            | `U8`  |         | _enabled_         |

_enabled_ will be set to 1 when the stream of [Replit Audio Server Frame
Request Message](#replit-audio-server-frame-request-message) messages will
start. _enabled_ will be set to 0 if client had not sent a [Start
Encoder](#replit-audio-client-start-encoder-message) message beforehand, or if
there was any other problem starting the stream. If there is an error at any
future point, or if the client sent a [Start
Encoder](#replit-audio-client-start-encoder-message) with the _enabled_ field
set to 0, the server will send an additional [Replit Audio Server Start
Continuous Updates
Message](#replit-audio-server-start-continuous-updates-message) with _enabled_
set to 0 after sending the last audio frame.
