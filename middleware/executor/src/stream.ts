import { Params } from './types/requestBody';
import { getStreamModeSplitPattern, type SplitPatternType } from './utils';

function isNetworkConnectionError(error: any): boolean {
  if (error?.name === 'AbortError') {
    return true;
  }
  const networkErrorCodes = ['UND_ERR_SOCKET'];
  return (
    networkErrorCodes.includes(error?.code) ||
    networkErrorCodes.includes(error?.cause?.code)
  );
}

/**
 * Iterate an upstream SSE byte stream, split it into events by `splitPattern`,
 * and yield each event — transformed by `transformFunction` when present
 * (which carries mutable `streamState` across chunks), otherwise re-emitted
 * with its delimiter.
 */
export async function* readStream(
  reader: ReadableStreamDefaultReader,
  splitPattern: SplitPatternType,
  transformFunction: Function | undefined,
  isSleepTimeRequired: boolean,
  fallbackChunkId: string,
  strictOpenAiCompliance: boolean,
  gatewayRequest: Params
) {
  let buffer = '';
  const decoder = new TextDecoder();
  let isFirstChunk = true;
  const streamState = {};

  while (true) {
    const { done, value } = await reader.read();
    if (done) {
      if (buffer.length > 0) {
        if (transformFunction) {
          yield transformFunction(
            buffer,
            fallbackChunkId,
            streamState,
            strictOpenAiCompliance,
            gatewayRequest
          );
        } else {
          yield buffer;
        }
      }
      break;
    }

    buffer += decoder.decode(value, { stream: true });
    // keep buffering until we have a complete chunk
    while (buffer.split(splitPattern).length > 1) {
      const parts = buffer.split(splitPattern);
      const lastPart = parts.pop() ?? ''; // remove the last part from the array and keep it in buffer
      for (const part of parts) {
        if (part.length > 0) {
          if (isFirstChunk) {
            isFirstChunk = false;
            await new Promise((resolve) => setTimeout(resolve, 25));
          } else if (isSleepTimeRequired) {
            await new Promise((resolve) => setTimeout(resolve, 1));
          }

          if (transformFunction) {
            const transformedChunk = transformFunction(
              part,
              fallbackChunkId,
              streamState,
              strictOpenAiCompliance,
              gatewayRequest
            );
            if (transformedChunk !== undefined) {
              yield transformedChunk;
            }
          } else {
            yield part + splitPattern;
          }
        }
      }

      buffer = lastPart; // keep the last part (after the last delimiter) in buffer
    }
  }
}

/**
 * Buffered response: run the provider's response transform over the parsed
 * upstream JSON and rebuild the response.
 */
export async function handleNonStreamingMode(
  response: Response,
  responseTransformer: Function | undefined,
  strictOpenAiCompliance: boolean,
  gatewayRequestUrl: string,
  gatewayRequest: Params
): Promise<Response> {
  if (!responseTransformer) {
    return new Response(response.body, response);
  }
  const responseBodyJson = responseTransformer(
    await response.json(),
    response.status,
    response.headers,
    strictOpenAiCompliance,
    gatewayRequestUrl,
    gatewayRequest
  );
  return new Response(JSON.stringify(responseBodyJson), response);
}

/**
 * Streaming response: transform each upstream SSE event through the provider's
 * stream transform and re-emit it, cancelling the upstream read if the
 * downstream client goes away.
 */
export function handleStreamingMode(
  response: Response,
  proxyProvider: string,
  responseTransformer: Function | undefined,
  requestURL: string,
  strictOpenAiCompliance: boolean,
  gatewayRequest: Params
): Response {
  const splitPattern = getStreamModeSplitPattern(proxyProvider, requestURL);
  // If the provider doesn't supply a completion id, we generate a fallback
  // id using the provider name + timestamp.
  const fallbackChunkId = `${proxyProvider}-${Date.now().toString()}`;

  if (!response.body) {
    throw new Error('Response format is invalid. Body not found');
  }
  const { readable, writable } = new TransformStream();
  const writer = writable.getWriter();
  const reader = response.body.getReader();
  const isSleepTimeRequired = false;
  const encoder = new TextEncoder();
  let downstreamClosed = false;
  let upstreamCancelled = false;
  let upstreamCompleted = false;

  const cancelUpstreamReader = async (reason?: unknown) => {
    if (upstreamCancelled) {
      return;
    }
    upstreamCancelled = true;
    try {
      await reader.cancel(reason);
    } catch (cancelError: any) {
      if (
        cancelError?.name === 'TypeError' &&
        cancelError?.message === 'terminated'
      ) {
        // Reader already torn down — nothing else to do.
        return;
      }
      console.error('Failed to cancel upstream reader:', proxyProvider, cancelError);
    }
  };

  writer.closed.catch((error) => {
    downstreamClosed = true;
    if (!upstreamCompleted) {
      cancelUpstreamReader(error).catch((cancelError: any) => {
        if (
          cancelError?.name === 'TypeError' &&
          cancelError?.message === 'terminated'
        ) {
          return;
        }
        console.error(
          'Error raised while cancelling upstream reader:',
          proxyProvider,
          cancelError
        );
      });
    }
  });

  const writeChunk = async (chunk: string | Uint8Array) => {
    if (chunk === undefined || downstreamClosed) {
      return;
    }
    const payload = chunk instanceof Uint8Array ? chunk : encoder.encode(chunk);
    try {
      await writer.write(payload);
    } catch (error) {
      downstreamClosed = true;
      if (!upstreamCompleted) {
        await cancelUpstreamReader(error);
      }
      throw error;
    }
  };

  void (async () => {
    try {
      for await (const chunk of readStream(
        reader,
        splitPattern,
        responseTransformer,
        isSleepTimeRequired,
        fallbackChunkId,
        strictOpenAiCompliance,
        gatewayRequest
      )) {
        await writeChunk(chunk);
      }
      upstreamCompleted = true;
    } catch (error) {
      if (!downstreamClosed && !isNetworkConnectionError(error)) {
        console.error('Error during stream processing:', proxyProvider, error);
      }
    } finally {
      if (!downstreamClosed) {
        try {
          await writer.close();
        } catch (closeError) {
          console.error('Failed to close the writer:', proxyProvider, closeError);
        }
      }
      if (!upstreamCompleted && !upstreamCancelled) {
        await cancelUpstreamReader();
      }
    }
  })();

  return new Response(readable, response);
}
