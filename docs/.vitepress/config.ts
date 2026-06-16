import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'niphas',
  description: 'Nix-native platform for Kubernetes',
  appearance: 'dark',

  themeConfig: {
    nav: [
      { text: 'Architecture', link: '/architecture/' },
      { text: 'Components', link: '/components/' },
      { text: 'Reference', link: '/reference/' },
    ],

    sidebar: {
      '/architecture/': [
        {
          text: 'Architecture',
          items: [
            { text: 'Overview', link: '/architecture/' },
            { text: 'Runtime Model', link: '/architecture/runtime-model' },
            { text: 'Deploy Flow', link: '/architecture/deploy-flow' },
          ],
        },
      ],
      '/components/': [
        {
          text: 'Components',
          items: [
            { text: 'Overview', link: '/components/' },
            { text: 'Operator', link: '/components/operator' },
            { text: 'Evaluator', link: '/components/eval' },
            { text: 'CSI Driver', link: '/components/csi-driver' },
            { text: 'Mesh Protocol', link: '/components/mesh-protocol' },
          ],
        },
      ],
      '/reference/': [
        {
          text: 'API & Formats',
          items: [
            { text: 'Overview', link: '/reference/' },
            { text: 'CRD Reference', link: '/reference/crd-reference' },
            { text: 'Nix Wire Format', link: '/reference/nix-wire' },
          ],
        },
        {
          text: 'Operations',
          items: [
            { text: 'Security Design', link: '/reference/security-design' },
            { text: 'Resilience', link: '/reference/resilience' },
            { text: 'Testing', link: '/reference/testing' },
          ],
        },
        {
          text: 'Other',
          items: [
            { text: 'Security Policy', link: '/reference/security' },
            { text: 'Known Issues', link: '/reference/problem' },
          ],
        },
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/fullzer4/niphas' },
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright 2024-present niphas contributors',
    },
  },
})
