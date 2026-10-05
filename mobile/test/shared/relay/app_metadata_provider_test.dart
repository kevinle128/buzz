import 'package:buzz/shared/community/relay_information_provider.dart';
import 'package:buzz/shared/relay/app_metadata_provider.dart';
import 'package:buzz/shared/relay/nostr_models.dart';
import 'package:buzz/shared/relay/relay_provider.dart';
import 'package:buzz/shared/relay/relay_session.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart' as http_testing;
import 'package:nostr/nostr.dart' as nostr;

const _appId = '6eb31227-8ed2-42ec-9024-863497cbeed2';

final _communityOne = nostr.Keys.generate();
final _communityTwo = nostr.Keys.generate();

NostrEvent _fromNostr(nostr.Event event) => NostrEvent.fromJson(event.toMap());

nostr.Event _signMetadata({
  required nostr.Keys keys,
  required String name,
  int createdAt = 1700000000,
}) {
  return nostr.Event.from(
    kind: EventKind.appMetadata,
    content: '',
    secretKey: keys.secret,
    createdAt: createdAt,
    tags: [
      ['d', _appId],
      ['name', name],
      ['status', 'active'],
    ],
    verify: true,
  );
}

void main() {
  test(
    'queries kind 39008 from the active relay self and folds heads',
    () async {
      final metadata = _fromNostr(
        _signMetadata(keys: _communityOne, name: 'Archon'),
      );
      final session = _AppsRelaySession({
        _communityOne.public.toLowerCase(): [metadata],
      });
      final container = ProviderContainer(
        overrides: [
          relayInformationHttpClientProvider.overrideWithValue(
            http_testing.MockClient(
              (_) async =>
                  http.Response('{"self":"${_communityOne.public}"}', 200),
            ),
          ),
          relayConfigProvider.overrideWith(
            () => _MutableRelayConfig('https://one.example.com'),
          ),
          relaySessionProvider.overrideWith(() => session),
        ],
      );
      addTearDown(container.dispose);
      final subscription = container.listen(appMetadataProvider, (_, _) {});
      addTearDown(subscription.close);

      final apps = await container.read(appMetadataProvider.future);

      expect(session.filters, hasLength(1));
      expect(session.filters.single.kinds, [EventKind.appMetadata]);
      expect(session.filters.single.authors, [
        _communityOne.public.toLowerCase(),
      ]);
      expect(session.filters.single.limit, 500);
      expect(apps[_appId]?.name, 'Archon');
    },
  );

  test(
    'discards the first community App map after the active community changes',
    () async {
      final first = _fromNostr(
        _signMetadata(keys: _communityOne, name: 'One Archon'),
      );
      final second = _fromNostr(
        _signMetadata(keys: _communityTwo, name: 'Two Archon'),
      );
      final session = _AppsRelaySession({
        _communityOne.public.toLowerCase(): [first],
        _communityTwo.public.toLowerCase(): [second],
      });
      final client = http_testing.MockClient((request) async {
        if (request.url.host == 'one.example.com') {
          return http.Response('{"self":"${_communityOne.public}"}', 200);
        }
        return http.Response('{"self":"${_communityTwo.public}"}', 200);
      });
      final container = ProviderContainer(
        overrides: [
          relayInformationHttpClientProvider.overrideWithValue(client),
          relayConfigProvider.overrideWith(
            () => _MutableRelayConfig('https://one.example.com'),
          ),
          relaySessionProvider.overrideWith(() => session),
        ],
      );
      addTearDown(container.dispose);
      final subscription = container.listen(appMetadataProvider, (_, _) {});
      addTearDown(subscription.close);

      expect(
        (await container.read(appMetadataProvider.future))[_appId]?.name,
        'One Archon',
      );

      container
          .read(relayConfigProvider.notifier)
          .update(baseUrl: 'https://two.example.com');

      final afterSwitch = await container.read(appMetadataProvider.future);
      expect(afterSwitch[_appId]?.name, 'Two Archon');
      expect(
        afterSwitch[_appId]?.relayPubkey,
        _communityTwo.public.toLowerCase(),
      );
      expect(afterSwitch.length, 1);
    },
  );
}

class _MutableRelayConfig extends RelayConfigNotifier {
  _MutableRelayConfig(this._baseUrl);
  final String _baseUrl;

  @override
  RelayConfig build() => RelayConfig(baseUrl: _baseUrl);
}

class _AppsRelaySession extends RelaySessionNotifier {
  _AppsRelaySession(this._eventsByAuthor);

  final Map<String, List<NostrEvent>> _eventsByAuthor;
  final List<NostrFilter> filters = [];

  @override
  SessionState build() => const SessionState(status: SessionStatus.connected);

  @override
  Future<List<NostrEvent>> fetchHistory(
    NostrFilter filter, {
    Duration timeout = const Duration(seconds: 8),
  }) async {
    filters.add(filter);
    final author = filter.authors?.single.toLowerCase();
    return [...?_eventsByAuthor[author]];
  }
}
