part of '../thread_detail_page.dart';

class _Avatar extends StatelessWidget {
  final UserProfile? profile;
  final TimelineMessage message;
  final bool isAgent;

  const _Avatar({
    required this.profile,
    required this.message,
    required this.isAgent,
  });

  @override
  Widget build(BuildContext context) {
    final appName = message.appDisplayName ?? 'App';
    final initial = message.isApp
        ? appName[0].toUpperCase()
        : profile?.initial ??
              (message.pubkey.isNotEmpty
                  ? message.pubkey[0].toUpperCase()
                  : '?');
    final avatarUrl = message.isApp ? message.appPicture : profile?.avatarUrl;

    return AvatarImage(
      imageUrl: avatarUrl,
      radius: messageAvatarSize / 2,
      backgroundColor: context.colors.primaryContainer,
      fallback: Text(
        initial,
        style: context.textTheme.labelMedium?.copyWith(
          color: context.colors.onPrimaryContainer,
          fontWeight: FontWeight.w600,
        ),
      ),
      isAgent: isAgent,
    );
  }
}
